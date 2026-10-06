//! Every store write, file write and hash is a yield, so another request
//! runs while one awaits. These tests drive the engine directly, so two
//! requests enter it in a known order and the races the loopback clients
//! can only make likely are certain. Tests that need two requests on two
//! threads, as on two workers of the listener, hold one at its clock read
//! (`WallClock::hold_next_read`) while the other runs.

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
    EngineDevice, ScriptedIntake, TestService, chunks, engine_hello, held_while, seeded_bytes,
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
    // Both requests passed the gate (the session was open at both heads)
    // and enter the engine at the same instant on two threads, as two
    // connections on two workers do: a barrier releases them together.
    // The second must find the session gone, not a save still in flight.
    // Each body is padded to the JSON limit, so the parse between the
    // check and the take is wide enough for the other thread to land in.
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

/// `POST /v1/pair` as `principal`, the gate's answer, with the body padded
/// by whitespace to the JSON limit: a legal request whose parse takes long
/// enough for the other thread to arrive.
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
    // The phone retries `complete` after its own timeout while the computer
    // is still copying a large file. A second admission would create a
    // second meeting; with the real intake it can also fail on the moved
    // source and overwrite the `complete` receipt with `failed`.
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
    // Verifying a 4 GiB upload takes seconds; `/v1/hello` and every other
    // connection's auth gate must not queue behind it.
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

/// A service that does not listen, a phone paired straight into the engine,
/// and the metadata and chunks of a two-chunk recording of that phone.
type Recording = (TestService, EngineDevice, RecordingMetadata, Vec<Vec<u8>>);

/// A [`Recording`] the phone has not announced yet, with `intake` in
/// place of the fake one when given.
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

/// A [`Recording`] the phone announced and sent in full, with `intake` in
/// place of the fake one when given.
async fn uploaded_two_chunks(seed: u64, intake: Option<Arc<ScriptedIntake>>) -> Recording {
    let recording = two_chunk_recording(seed, intake).await;
    let (_, phone, metadata, chunks) = &recording;
    phone.upload_all(metadata, &chunks.concat()).await;
    recording
}

/// The receipt memory and the store hold for `recording_id`.
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

/// The chunk set memory and the store hold for `recording_id`.
fn chunk_sets(test: &TestService, recording_id: Uuid) -> (Vec<i64>, Vec<i64>) {
    let (in_memory, stored) = receipts(test, recording_id);
    (in_memory.received_chunks, stored.received_chunks)
}

/// The device memory and the store hold `recording_id` for.
fn owners(test: &TestService, recording_id: Uuid) -> (Uuid, Uuid) {
    let (in_memory, stored) = receipts(test, recording_id);
    (in_memory.device_id, stored.device_id)
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

/// Memory and the store hold `recording_id` as `complete` with
/// `meeting_id`, the intake admitted it once, and the phone's next
/// `complete` answers that meeting again.
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
    // The phone keeps two chunks in flight, and the listener runs each on
    // a worker of its own. Chunk 0 is held at its clock read, after its
    // file write, while chunk 1 lands in full. Chunk 0 then folds into the
    // receipt as memory holds it, with chunk 1 in it. A fold that read the
    // receipt before the clock and wrote it back after would put back the
    // copy without chunk 1, in memory and in the store.
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
    // The phone announces again (a retry after its own timeout) while its
    // next chunk is in flight. The announce is held at its clock read,
    // after it read the receipt, while chunk 1 lands. Its `receiving`
    // write then changes the receipt as memory holds it, chunk 1 and all.
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
    // Chunk 0 is held at its clock read, after its file write, while the
    // phone is revoked (its receipt goes) and another phone announces the
    // same recording id. The fold then finds a receipt of another device
    // and changes nothing: 404, and the new receipt lists no chunk.
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
    // The partial went (a sweep), so the phone's announce opens a new one
    // and empties the chunk set. It is held at its clock read while the
    // phone is revoked and another phone announces the same recording id
    // and sends chunk 0. Its `receiving` write then finds a receipt of
    // another device and changes nothing, so that phone's chunk stays.
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
    // The phone announces again (a retry after its own timeout) while its
    // `complete` admits the recording. The announce is held at its clock
    // read, after it read the receipt as `receiving`, while the `complete`
    // runs to the end. Its `receiving` write then finds the receipt
    // `complete` in memory, changes nothing and answers with that. Written
    // back, `receiving` would send the phone's next `complete` back to the
    // start, and the upload after it would become a second meeting.
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
    // The phone sent chunk 1 again after its own timeout while the first
    // attempt was still in flight. That attempt is held at its clock read,
    // after its file write, while the second one lands and the phone's
    // `complete` runs to the end. Its fold then finds the receipt
    // `complete` in memory and changes nothing, and the chunk is answered
    // as received.
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
    // The phone announces a new recording twice at once (a retry after its
    // own timeout). One announce is held at its clock read, after its
    // receipt read found nothing and it opened the partial, while the other
    // one answers 201 and chunk 0 lands. The held one then finds that
    // receipt in memory and answers as a re-announce, so chunk 0 stays.
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
async fn an_announce_of_another_phone_that_found_no_receipt_is_refused() {
    // Two phones announce the same recording id at once. Phone B's announce
    // is held at its clock read, after its receipt read found nothing,
    // while phone A's announce answers 201 and chunk 0 lands. B's then finds
    // A's receipt in memory and is refused as a re-announce of another
    // device's recording, and the receipt stays A's.
    let (test, phone, metadata, chunks) = two_chunk_recording(69, None).await;
    let id = metadata.recording_id;
    let other = EngineDevice::paired(&test, "Other iPhone").await;
    let second = {
        let metadata = metadata.clone();
        async move { other.announce(&metadata).await }
    };
    let (refused, ()) = held_while(&test, second, async {
        assert_eq!(phone.announce(&metadata).await.status.as_u16(), 201);
        assert_eq!(phone.upload(id, 0, &chunks[0]).await.status.as_u16(), 204);
    })
    .await;
    assert_eq!(refused.status.as_u16(), 409);

    assert_eq!(owners(&test, id), (phone.device.id, phone.device.id));
    assert_eq!(chunk_sets(&test, id), (vec![0], vec![0]));
}

#[tokio::test]
async fn a_complete_of_a_revoked_phone_stays_out_of_another_phones_receipt() {
    // The phone is revoked while its `complete` is in the intake, and
    // another phone announces the same recording id. The intake then
    // answers, and the `complete` write finds a receipt of another device
    // in memory and changes nothing. Written into it, it would tell the
    // other phone `complete` for a recording never admitted from it, and
    // that phone would delete its copy.
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
    // The intake took the verified file, as the real one does, and the
    // phone announces again. The announce finds no file, opens a new
    // partial and sidecar, and is held at its clock read while the
    // `complete` runs to the end. Its `receiving` write then finds the
    // receipt `complete`, and the files it opened go: a stale `complete`
    // would find them and start a verify of an empty partial.
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
    // The phone is revoked while its `complete` is in the intake, and
    // another phone announces the same recording id, which opens a partial
    // and a sidecar. The intake then answers, and the admission leaves
    // those files alone: they are the other phone's upload, which goes on
    // to its own admission. Removed, its `complete` would answer 404
    // ("announce again") and the phone would send every chunk again.
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

    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(
            other.upload(id, index as i64, chunk).await.status.as_u16(),
            204
        );
    }
    // The gated intake lets the other phone's admission through at once.
    intake.release();
    assert_eq!(completed(&other, id).await, meeting_id());
    assert_eq!(intake.count(), 2);
}

/// Two first announces of one recording at once: the receipt's, of `phone`
/// with `metadata`, and a late one of `late` with another `format`. Each
/// is held at its clock read, after its receipt read found nothing, the
/// receipt's first. The receipt's goes on first and makes the receipt, and
/// the late one then finds it. Before, each had written its sidecar by its
/// clock read, the late one last. Returns the two answers.
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
    let (first_hold, first_thread) = common::held(test, {
        let (phone, metadata) = (phone.clone(), metadata.clone());
        async move { phone.announce(&metadata).await }
    });
    let (late_hold, late_thread) = common::held(test, {
        let late = late.clone();
        async move { late.announce(&late_metadata).await }
    });
    first_hold.release();
    let first = first_thread.join().unwrap();
    late_hold.release();
    (first, late_thread.join().unwrap())
}

/// The sidecar is the metadata of the announce that made the receipt, and
/// the phone's upload is admitted with it.
async fn the_sidecar_is_the_receipts(
    test: &TestService,
    phone: &EngineDevice,
    metadata: &RecordingMetadata,
    chunks: &[Vec<u8>],
) {
    let id = metadata.recording_id;
    assert_eq!(owners(test, id), (phone.device.id, phone.device.id));
    assert_eq!(test.inbox().load_metadata(id).as_ref(), Some(metadata));
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(
            phone.upload(id, index as i64, chunk).await.status.as_u16(),
            204
        );
    }
    completed(phone, id).await;
    let admissions = test.intake.admissions.entries();
    assert_eq!(admissions.len(), 1);
    assert_eq!(&admissions[0].metadata, metadata);
}

#[tokio::test]
async fn racing_first_announces_with_another_format_keep_the_receipts_sidecar() {
    // The phone announces a new recording twice at once, the second time
    // with another format. Only the announce that made the receipt opens
    // the files; the late one answers as a re-announce and leaves the
    // sidecar alone. Before, each announce wrote its sidecar before it made
    // the receipt, so the late one's `format` reached the intake with the
    // other one's receipt.
    let (test, phone, metadata, chunks) = two_chunk_recording(75, None).await;
    let (first, late) = racing_first_announces(&test, &phone, &phone, &metadata);
    assert_eq!(first.status.as_u16(), 201);
    assert_eq!(late.status.as_u16(), 200);

    the_sidecar_is_the_receipts(&test, &phone, &metadata, &chunks).await;
}

#[tokio::test]
async fn racing_first_announces_of_two_phones_keep_the_receipts_sidecar() {
    // Two phones announce the same recording id at once, with other
    // formats. The late one is refused as another device's recording and
    // leaves the sidecar of the one that made the receipt alone.
    let (test, phone, metadata, chunks) = two_chunk_recording(76, None).await;
    let other = EngineDevice::paired(&test, "Other iPhone").await;
    let (first, late) = racing_first_announces(&test, &phone, &other, &metadata);
    assert_eq!(first.status.as_u16(), 201);
    assert_eq!(late.status.as_u16(), 409);

    the_sidecar_is_the_receipts(&test, &phone, &metadata, &chunks).await;
}

#[tokio::test]
async fn an_announce_whose_files_cannot_be_opened_keeps_its_receipt() {
    // Only the announce that made the receipt opens the files, once the
    // receipt is made. When the opening fails, the receipt is saved anyway,
    // at the place in line it took, and the answer is 500. The phone's
    // retried announce finds the receipt and opens the files as a
    // re-announce.
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
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(
            phone.upload(id, index as i64, chunk).await.status.as_u16(),
            204
        );
    }
    completed(&phone, id).await;
    assert_eq!(test.intake.admissions.count(), 1);
}

#[tokio::test]
async fn a_reannounce_during_the_intake_leaves_no_file_after_the_admission() {
    // The intake took the verified file, as the real one does, and the
    // phone announces again (a retry after its own timeout). The announce
    // finds no file, opens a new partial and sidecar and answers. The
    // admission then removes every file of the recording: an empty partial
    // left behind would wait for the next start's sweep.
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
