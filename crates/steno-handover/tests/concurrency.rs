//! Races between requests in the engine, driven directly so they are
//! certain: every store write, file write and hash is a yield.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::sync::Arc;

use common::{
    EngineDevice, ScriptedIntake, TestService, chunks, engine_hello, held, held_while, seeded_bytes,
};
use steno_core::{
    AudioFormat, HandoverReceipt, HandoverState, HandoverStateKind, RecordingMetadata,
};
use steno_handover::engine::{HandoverRequest, HandoverResponse, Principal, RequestHandling as _};
use steno_handover::route::Route;
use steno_handover::{HandoverConfiguration, wire};
use uuid::Uuid;

fn meeting_id() -> Uuid {
    Uuid::parse_str("C0C0C0C0-0000-4000-8000-000000000001").unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pairing_secret_pairs_exactly_once_under_concurrent_use() {
    // Two requests that passed the gate enter the engine at once; the second
    // must find the session gone.
    let test = TestService::with(common::Options {
        start: false,
        ..common::Options::default()
    })
    .await;
    let runtime = tokio::runtime::Handle::current();
    for round in 0..ROUNDS {
        let payload = test.service.begin_pairing();
        let principal = common::pairing_principal(&test.service, &payload).await;
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let racers = [
            (Uuid::new_v4(), "Nicolai's iPhone"),
            (Uuid::new_v4(), "Photographed QR"),
        ]
        .map(|(device_id, device_name)| {
            let request = padded_pair_request(principal.clone(), device_id, device_name);
            let (service, barrier, runtime) =
                (test.service.clone(), barrier.clone(), runtime.clone());
            tokio::task::spawn_blocking(move || {
                barrier.wait();
                runtime.block_on(service.engine.handle(request))
            })
        });
        let mut statuses = Vec::new();
        for racer in racers {
            statuses.push(racer.await.unwrap().status.as_u16());
        }
        statuses.sort_unstable();
        assert_eq!(
            statuses,
            vec![200, 403],
            "round {round}: one phone pairs, not two"
        );
        assert!(
            !test.service.engine.pairing_is_open(),
            "round {round}: the secret is spent"
        );
    }
    let devices = test.service.paired_devices().await.unwrap();
    assert_eq!(devices.len(), ROUNDS, "one device per window");
}

const ROUNDS: usize = 100;

/// `POST /v1/pair` as `principal`, padded to the JSON limit so its parse is slow.
fn padded_pair_request(
    principal: Principal,
    device_id: Uuid,
    device_name: &str,
) -> HandoverRequest {
    let json = serde_json::to_vec(&wire::PairRequest {
        device_id,
        device_name: device_name.to_owned(),
    })
    .unwrap();
    let padding = usize::try_from(HandoverConfiguration::JSON_BODY_LIMIT).unwrap() - json.len();
    let mut body = Vec::with_capacity(json.len() + padding);
    body.push(b'{');
    body.resize(1 + padding, b' ');
    body.extend_from_slice(&json[1..]);
    HandoverRequest::new(Route::Pair, principal).with_body(body)
}

#[tokio::test]
async fn concurrent_completes_admit_once_and_keep_the_complete_receipt() {
    // A `complete` retried while the intake still copies must not admit a
    // second meeting or overwrite the receipt with `failed`.
    let chunk_size: i64 = 64 * 1024;
    let intake = ScriptedIntake::gated(meeting_id(), true);
    let test = TestService::with(common::Options {
        chunk_size,
        intake: Some(intake.clone() as Arc<dyn steno_core::HandoverIntake>),
        start: false,
        ..common::Options::default()
    })
    .await;
    let phone = EngineDevice::paired(&test, "Direct iPhone").await;
    let bytes = seeded_bytes(2 * chunk_size as usize, 61);
    let metadata = phone.metadata(&bytes, chunk_size);
    phone.upload_all(&metadata, &bytes).await;
    let id = metadata.recording_id;

    let first = phone.complete(id);
    let second = async {
        intake.admitting().await;
        let retry = common::signalled("the retry answers", phone.complete(id)).await;
        intake.release();
        retry
    };
    let (a, b) = tokio::join!(first, second);

    let mut statuses = vec![a.status.as_u16(), b.status.as_u16()];
    statuses.sort_unstable();
    assert_eq!(
        statuses,
        vec![200, 409],
        "one admits, the retry is told to wait"
    );
    let conflict = [&a, &b]
        .into_iter()
        .find(|r| r.status.as_u16() == 409)
        .unwrap();
    assert_eq!(
        conflict.decode::<wire::RecordingStatus>().unwrap(),
        wire::RecordingStatus {
            state: HandoverStateKind::Verifying,
            received_chunks: vec![0, 1]
        },
        "the 409 status lists every chunk, so the phone backs off instead of re-uploading"
    );
    assert_eq!(intake.count(), 1);
    assert_eq!(
        test.store.handover_receipt(id).unwrap().unwrap().state,
        HandoverState::Complete {
            meeting_id: meeting_id()
        }
    );

    // The phone's next try after the backoff: same id, no new admission.
    let third = phone.complete(id).await;
    assert_eq!(third.status.as_u16(), 200);
    assert_eq!(
        third.decode::<wire::CompleteResponse>().unwrap().meeting_id,
        meeting_id()
    );
    assert_eq!(intake.count(), 1);
}

#[tokio::test]
async fn the_gate_answers_while_a_whole_file_hash_runs() {
    // A long verify must not queue `/v1/hello` or another connection's gate.
    let chunk_size: i64 = 16 * 1024 * 1024;
    let test = TestService::with(common::Options {
        chunk_size,
        start: false,
        ..common::Options::default()
    })
    .await;
    let phone = EngineDevice::paired(&test, "Direct iPhone").await;
    let bytes = vec![0x5Au8; 8 * chunk_size as usize];
    let metadata = phone.metadata(&bytes, chunk_size);
    phone.upload_all(&metadata, &bytes).await;
    let id = metadata.recording_id;
    let inbox = test.inbox();
    let mut receipts = test.service.receipts();

    let completion = phone.complete(id);
    let observed = async {
        // `verifying` is written just before the hash starts.
        loop {
            let seen = receipts.borrow_and_update().iter().any(|receipt| {
                receipt.recording_id == id && receipt.state.kind() == HandoverStateKind::Verifying
            });
            if seen {
                break;
            }
            receipts.changed().await.unwrap();
        }
        let hello = engine_hello(&test).await;
        assert_eq!(hello.status.as_u16(), 200);
        assert!(
            inbox.has_partial(id) && !inbox.has_verified(id, AudioFormat::M4aAac),
            "hello was answered while the hash was still running, before the promote"
        );
    };
    let (completed, ()) = tokio::join!(completion, observed);
    assert_eq!(completed.status.as_u16(), 200);
    assert_eq!(test.intake.admissions.count(), 1);
}

/// A non-listening service, a paired phone and its two-chunk recording.
type Recording = (TestService, EngineDevice, RecordingMetadata, Vec<Vec<u8>>);

/// A [`Recording`] not announced yet, with `intake` in place of the fake one.
async fn two_chunk_recording(seed: u64, intake: Option<Arc<ScriptedIntake>>) -> Recording {
    let chunk_size: i64 = 64 * 1024;
    let test = TestService::with(common::Options {
        chunk_size,
        intake: intake.map(|intake| intake as Arc<dyn steno_core::HandoverIntake>),
        start: false,
        ..common::Options::default()
    })
    .await;
    let phone = EngineDevice::paired(&test, "Direct iPhone").await;
    let bytes = seeded_bytes(2 * chunk_size as usize, seed);
    let metadata = phone.metadata(&bytes, chunk_size);
    (test, phone, metadata, chunks(&bytes, chunk_size))
}

/// A [`Recording`] the phone announced.
async fn announced_two_chunks(seed: u64) -> Recording {
    let recording = two_chunk_recording(seed, None).await;
    let (_, phone, metadata, _) = &recording;
    assert_eq!(phone.announce(metadata).await.status.as_u16(), 201);
    recording
}

/// A [`Recording`] announced and sent in full, with `intake` if given.
async fn uploaded_two_chunks(seed: u64, intake: Option<Arc<ScriptedIntake>>) -> Recording {
    let recording = two_chunk_recording(seed, intake).await;
    let (_, phone, metadata, chunks) = &recording;
    phone.upload_all(metadata, &chunks.concat()).await;
    recording
}

fn receipts(test: &TestService, recording_id: Uuid) -> (HandoverReceipt, HandoverReceipt) {
    let in_memory = test
        .service
        .engine
        .receipts_snapshot()
        .into_iter()
        .find(|receipt| receipt.recording_id == recording_id)
        .expect("the receipt is in memory");
    let stored = test
        .store
        .handover_receipt(recording_id)
        .unwrap()
        .expect("the receipt is stored");
    (in_memory, stored)
}

fn chunk_sets(test: &TestService, recording_id: Uuid) -> (Vec<i64>, Vec<i64>) {
    let (in_memory, stored) = receipts(test, recording_id);
    (in_memory.received_chunks, stored.received_chunks)
}

fn owners(test: &TestService, recording_id: Uuid) -> (Uuid, Uuid) {
    let (in_memory, stored) = receipts(test, recording_id);
    (in_memory.device_id, stored.device_id)
}

/// Sends every chunk of `recording_id`, each answered 204.
async fn send_all(phone: &EngineDevice, recording_id: Uuid, chunks: &[Vec<u8>]) {
    for (index, chunk) in chunks.iter().enumerate() {
        let response = phone.upload(recording_id, index as i64, chunk).await;
        assert_eq!(response.status.as_u16(), 204, "chunk {index}");
    }
}

/// The meeting a `complete` of `recording_id` admitted.
async fn completed(phone: &EngineDevice, recording_id: Uuid) -> Uuid {
    let completed = phone.complete(recording_id).await;
    assert_eq!(completed.status.as_u16(), 200);
    completed
        .decode::<wire::CompleteResponse>()
        .unwrap()
        .meeting_id
}

/// `recording_id` is `complete` with `meeting_id`, admitted once.
async fn stays_complete(
    test: &TestService,
    phone: &EngineDevice,
    recording_id: Uuid,
    meeting_id: Uuid,
) {
    let (in_memory, stored) = receipts(test, recording_id);
    let complete = HandoverState::Complete { meeting_id };
    assert_eq!(
        (in_memory.state, stored.state),
        (complete.clone(), complete)
    );
    assert_eq!(test.intake.admissions.count(), 1);
    assert_eq!(completed(phone, recording_id).await, meeting_id);
    assert_eq!(test.intake.admissions.count(), 1);
}

#[tokio::test]
async fn two_chunks_that_land_at_once_both_stay_in_the_receipt() {
    // Chunk 0 is held at its clock read while chunk 1 lands; its fold must
    // keep chunk 1, in memory and in the store.
    let (test, phone, metadata, chunks) = announced_two_chunks(63).await;
    let id = metadata.recording_id;
    let first = {
        let (phone, chunk) = (phone.clone(), chunks[0].clone());
        async move { phone.upload(id, 0, &chunk).await }
    };
    let (first, second) = held_while(&test, first, phone.upload(id, 1, &chunks[1])).await;
    assert_eq!(second.status.as_u16(), 204);
    assert_eq!(first.status.as_u16(), 204);

    assert_eq!(chunk_sets(&test, id), (vec![0, 1], vec![0, 1]));
}

#[tokio::test]
async fn a_reannounce_keeps_a_chunk_that_lands_while_it_runs() {
    // A re-announce held while chunk 1 lands keeps chunk 1.
    let (test, phone, metadata, chunks) = announced_two_chunks(64).await;
    let id = metadata.recording_id;
    assert_eq!(phone.upload(id, 0, &chunks[0]).await.status.as_u16(), 204);
    let reannounce = {
        let phone = phone.clone();
        async move { phone.announce(&metadata).await }
    };
    let (reannounced, uploaded) =
        held_while(&test, reannounce, phone.upload(id, 1, &chunks[1])).await;
    assert_eq!(uploaded.status.as_u16(), 204);
    assert_eq!(reannounced.status.as_u16(), 200);

    assert_eq!(chunk_sets(&test, id), (vec![0, 1], vec![0, 1]));
}

#[tokio::test]
async fn a_late_chunk_of_a_revoked_phone_stays_out_of_another_phones_receipt() {
    // Chunk 0 held while the phone is revoked and another announces the id: 404,
    // and the new receipt is empty.
    let (test, phone, metadata, chunks) = announced_two_chunks(65).await;
    let id = metadata.recording_id;
    let late = {
        let (phone, chunk) = (phone.clone(), chunks[0].clone());
        async move { phone.upload(id, 0, &chunk).await }
    };
    let (late, ()) = held_while(&test, late, async {
        test.service.revoke(phone.device.id).await.unwrap();
        let other = EngineDevice::paired(&test, "Other iPhone").await;
        assert_eq!(other.announce(&metadata).await.status.as_u16(), 201);
    })
    .await;
    assert_eq!(late.status.as_u16(), 404);

    assert_eq!(chunk_sets(&test, id), (vec![], vec![]));
}

#[tokio::test]
async fn a_reannounce_of_a_revoked_phone_stays_out_of_another_phones_receipt() {
    // A held re-announce leaves the chunk another phone sent after a revoke.
    let (test, phone, metadata, chunks) = announced_two_chunks(72).await;
    let id = metadata.recording_id;
    std::fs::remove_file(test.inbox().partial(id)).unwrap();
    let reannounce = {
        let (phone, metadata) = (phone.clone(), metadata.clone());
        async move { phone.announce(&metadata).await }
    };
    let (reannounced, other) = held_while(&test, reannounce, async {
        test.service.revoke(phone.device.id).await.unwrap();
        let other = EngineDevice::paired(&test, "Other iPhone").await;
        assert_eq!(other.announce(&metadata).await.status.as_u16(), 201);
        assert_eq!(other.upload(id, 0, &chunks[0]).await.status.as_u16(), 204);
        other
    })
    .await;
    assert_eq!(reannounced.status.as_u16(), 200);

    assert_eq!(owners(&test, id), (other.device.id, other.device.id));
    assert_eq!(chunk_sets(&test, id), (vec![0], vec![0]));
}

#[tokio::test]
async fn a_reannounce_during_a_complete_leaves_the_receipt_complete() {
    // A re-announce held while `complete` admits answers `complete`, or the
    // next upload would become a second meeting.
    let (test, phone, metadata, _) = uploaded_two_chunks(66, None).await;
    let id = metadata.recording_id;
    let reannounce = {
        let phone = phone.clone();
        async move { phone.announce(&metadata).await }
    };
    let (reannounced, meeting_id) = held_while(&test, reannounce, completed(&phone, id)).await;
    assert_eq!(reannounced.status.as_u16(), 200);
    assert_eq!(
        reannounced.decode::<wire::RecordingStatus>().unwrap(),
        wire::RecordingStatus {
            state: HandoverStateKind::Complete,
            received_chunks: vec![0, 1]
        }
    );

    stays_complete(&test, &phone, id, meeting_id).await;
}

#[tokio::test]
async fn a_chunk_that_lands_during_a_complete_leaves_the_receipt_complete() {
    // A retried chunk held while `complete` runs is answered as received.
    let (test, phone, metadata, chunks) = announced_two_chunks(67).await;
    let id = metadata.recording_id;
    assert_eq!(phone.upload(id, 0, &chunks[0]).await.status.as_u16(), 204);
    let first = {
        let (phone, chunk) = (phone.clone(), chunks[1].clone());
        async move { phone.upload(id, 1, &chunk).await }
    };
    let (first, meeting_id) = held_while(&test, first, async {
        assert_eq!(phone.upload(id, 1, &chunks[1]).await.status.as_u16(), 204);
        completed(&phone, id).await
    })
    .await;
    assert_eq!(first.status.as_u16(), 204);

    stays_complete(&test, &phone, id, meeting_id).await;
}

#[tokio::test]
async fn an_announce_that_found_no_receipt_keeps_the_one_made_meanwhile() {
    // The held one of two first announces answers as a re-announce.
    let (test, phone, metadata, chunks) = two_chunk_recording(68, None).await;
    let id = metadata.recording_id;
    let second = {
        let (phone, metadata) = (phone.clone(), metadata.clone());
        async move { phone.announce(&metadata).await }
    };
    let (reannounced, ()) = held_while(&test, second, async {
        assert_eq!(phone.announce(&metadata).await.status.as_u16(), 201);
        assert_eq!(phone.upload(id, 0, &chunks[0]).await.status.as_u16(), 204);
    })
    .await;
    assert_eq!(reannounced.status.as_u16(), 200);
    assert_eq!(
        reannounced.decode::<wire::RecordingStatus>().unwrap(),
        wire::RecordingStatus {
            state: HandoverStateKind::Receiving,
            received_chunks: vec![0]
        }
    );

    assert_eq!(chunk_sets(&test, id), (vec![0], vec![0]));
}

#[tokio::test]
async fn an_announce_of_another_phone_that_found_no_receipt_takes_the_receipt_over() {
    // Two phones, same id and bytes: B, held, takes A's receipt over.
    let (test, phone, metadata, chunks) = two_chunk_recording(69, None).await;
    let id = metadata.recording_id;
    let other = EngineDevice::paired(&test, "Other iPhone").await;
    let second = {
        let metadata = metadata.clone();
        async move { other.announce(&metadata).await }
    };
    let (taken, ()) = held_while(&test, second, async {
        assert_eq!(phone.announce(&metadata).await.status.as_u16(), 201);
        assert_eq!(phone.upload(id, 0, &chunks[0]).await.status.as_u16(), 204);
    })
    .await;
    assert_eq!(taken.status.as_u16(), 200);
    assert_eq!(
        taken
            .decode::<wire::RecordingStatus>()
            .unwrap()
            .received_chunks,
        [0]
    );

    let other_id = owners(&test, id).0;
    assert_ne!(other_id, phone.device.id);
    assert_eq!(owners(&test, id), (other_id, other_id));
    assert_eq!(chunk_sets(&test, id), (vec![0], vec![0]));
}

#[tokio::test]
async fn a_complete_of_a_revoked_phone_stays_out_of_another_phones_receipt() {
    // After a revoke mid-intake, the `complete` write leaves another phone's
    // receipt for the same id alone.
    let intake = ScriptedIntake::gated(meeting_id(), false);
    let (test, phone, metadata, _) = uploaded_two_chunks(70, Some(intake.clone())).await;
    let id = metadata.recording_id;

    let completion = phone.complete(id);
    let meanwhile = async {
        intake.admitting().await;
        test.service.revoke(phone.device.id).await.unwrap();
        let other = EngineDevice::paired(&test, "Other iPhone").await;
        assert_eq!(other.announce(&metadata).await.status.as_u16(), 201);
        intake.release();
        other
    };
    let (answer, other) = tokio::join!(completion, meanwhile);
    assert_eq!(answer.status.as_u16(), 200);

    assert_eq!(owners(&test, id), (other.device.id, other.device.id));
    let (in_memory, stored) = receipts(&test, id);
    assert_eq!(
        (in_memory.state, stored.state),
        (HandoverState::Receiving, HandoverState::Receiving)
    );
}

#[tokio::test]
async fn a_reannounce_during_an_admission_leaves_no_files_behind() {
    // A re-announce held while `complete` finishes finds it `complete`, and its
    // new files go.
    let intake = ScriptedIntake::gated(meeting_id(), false);
    let (test, phone, metadata, _) = uploaded_two_chunks(71, Some(intake.clone())).await;
    let id = metadata.recording_id;
    let inbox = test.inbox();

    let completion = {
        let phone = phone.clone();
        tokio::spawn(async move { phone.complete(id).await })
    };
    intake.admitting().await;
    assert!(!inbox.has_verified(id, metadata.format));
    let reannounce = {
        let phone = phone.clone();
        async move { phone.announce(&metadata).await }
    };
    let (reannounced, answer) = held_while(&test, reannounce, async {
        intake.release();
        completion.await.unwrap()
    })
    .await;
    assert_eq!(answer.status.as_u16(), 200);
    assert_eq!(
        reannounced.decode::<wire::RecordingStatus>().unwrap().state,
        HandoverStateKind::Complete
    );

    assert!(!inbox.has_partial(id), "the new partial went");
    assert!(inbox.load_metadata(id).is_none(), "the new sidecar went");
}

#[tokio::test]
async fn a_complete_of_a_revoked_phone_leaves_another_phones_upload_alone() {
    // After a revoke mid-intake, the admission leaves another phone's files
    // for the same id alone.
    let intake = ScriptedIntake::gated(meeting_id(), false);
    let (test, phone, metadata, chunks) = uploaded_two_chunks(73, Some(intake.clone())).await;
    let id = metadata.recording_id;
    let inbox = test.inbox();

    let completion = phone.complete(id);
    let meanwhile = async {
        intake.admitting().await;
        test.service.revoke(phone.device.id).await.unwrap();
        let other = EngineDevice::paired(&test, "Other iPhone").await;
        assert_eq!(other.announce(&metadata).await.status.as_u16(), 201);
        intake.release();
        other
    };
    let (answer, other) = tokio::join!(completion, meanwhile);
    assert_eq!(answer.status.as_u16(), 200);
    assert!(inbox.has_partial(id), "the other phone's partial stays");
    assert_eq!(
        inbox.load_metadata(id),
        Some(metadata.clone()),
        "and so does its sidecar"
    );

    send_all(&other, id, &chunks).await;
    // Released ahead, the gated intake lets the other admission through.
    intake.release();
    assert_eq!(completed(&other, id).await, meeting_id());
    assert_eq!(intake.count(), 2);
}

/// Two first announces of one recording, by `phone` and by `late` with
/// another `format`, each held after its receipt read.
fn racing_first_announces(
    test: &TestService,
    phone: &EngineDevice,
    late: &EngineDevice,
    metadata: &RecordingMetadata,
) -> (HandoverResponse, HandoverResponse) {
    let late_metadata = RecordingMetadata {
        format: AudioFormat::Wav16kInt16,
        device_name: late.device.name.clone(),
        ..metadata.clone()
    };
    let (first_hold, first_thread) = held(test, {
        let (phone, metadata) = (phone.clone(), metadata.clone());
        async move { phone.announce(&metadata).await }
    });
    let (late_hold, late_thread) = held(test, {
        let late = late.clone();
        async move { late.announce(&late_metadata).await }
    });
    first_hold.release();
    let first = first_thread.join().unwrap();
    late_hold.release();
    (first, late_thread.join().unwrap())
}

/// The sidecar is the receipt maker's, and `phone`'s upload is admitted with it.
async fn the_sidecar_is_the_receipts(
    test: &TestService,
    phone: &EngineDevice,
    metadata: &RecordingMetadata,
    chunks: &[Vec<u8>],
) {
    let id = metadata.recording_id;
    assert_eq!(owners(test, id), (phone.device.id, phone.device.id));
    assert_eq!(test.inbox().load_metadata(id).as_ref(), Some(metadata));
    send_all(phone, id, chunks).await;
    completed(phone, id).await;
    let admissions = test.intake.admissions.entries();
    assert_eq!(admissions.len(), 1);
    assert_eq!(&admissions[0].metadata, metadata);
}

#[tokio::test]
async fn racing_first_announces_with_another_format_keep_the_receipts_sidecar() {
    // Only the receipt maker opens the files; the late one leaves the sidecar.
    let (test, phone, metadata, chunks) = two_chunk_recording(75, None).await;
    let (first, late) = racing_first_announces(&test, &phone, &phone, &metadata);
    assert_eq!(first.status.as_u16(), 201);
    assert_eq!(late.status.as_u16(), 200);

    the_sidecar_is_the_receipts(&test, &phone, &metadata, &chunks).await;
}

#[tokio::test]
async fn racing_first_announces_of_two_phones_keep_the_receipts_sidecar() {
    // The late phone takes the receipt over and leaves the maker's sidecar.
    let (test, phone, metadata, chunks) = two_chunk_recording(76, None).await;
    let other = EngineDevice::paired(&test, "Other iPhone").await;
    let (first, late) = racing_first_announces(&test, &phone, &other, &metadata);
    assert_eq!(first.status.as_u16(), 201);
    assert_eq!(late.status.as_u16(), 200);

    the_sidecar_is_the_receipts(&test, &other, &metadata, &chunks).await;
}

#[tokio::test]
async fn an_announce_whose_files_cannot_be_opened_keeps_its_receipt() {
    // A failed file open still saves the receipt and answers 500; the retry
    // opens the files.
    let (test, phone, metadata, chunks) = two_chunk_recording(78, None).await;
    let id = metadata.recording_id;
    let directory = test.inbox().directory.clone();
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::write(&directory, b"not a directory").unwrap();
    assert_eq!(phone.announce(&metadata).await.status.as_u16(), 500);
    let (in_memory, stored) = receipts(&test, id);
    assert_eq!(
        (in_memory.state, stored.state),
        (HandoverState::Receiving, HandoverState::Receiving)
    );

    std::fs::remove_file(&directory).unwrap();
    assert_eq!(phone.announce(&metadata).await.status.as_u16(), 200);
    assert_eq!(test.inbox().load_metadata(id), Some(metadata.clone()));
    send_all(&phone, id, &chunks).await;
    completed(&phone, id).await;
    assert_eq!(test.intake.admissions.count(), 1);
}

#[tokio::test]
async fn an_admission_removes_the_files_a_reannounce_opened_during_the_intake() {
    // The admission removes the files a re-announce opened.
    let intake = ScriptedIntake::gated(meeting_id(), false);
    let (test, phone, metadata, _) = uploaded_two_chunks(77, Some(intake.clone())).await;
    let id = metadata.recording_id;
    let inbox = test.inbox();

    let completion = phone.complete(id);
    let meanwhile = async {
        intake.admitting().await;
        let reannounced = phone.announce(&metadata).await;
        assert!(inbox.has_partial(id), "the announce opened a partial");
        intake.release();
        reannounced
    };
    let (answer, reannounced) = tokio::join!(completion, meanwhile);
    assert_eq!(answer.status.as_u16(), 200);
    assert_eq!(reannounced.status.as_u16(), 200);

    assert!(
        !inbox.recording_ids().contains(&id),
        "no file of the recording is left"
    );
}
