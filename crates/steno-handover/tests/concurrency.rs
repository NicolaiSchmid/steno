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

use common::{EngineDevice, ScriptedIntake, TestService, chunks, engine_hello, seeded_bytes};
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

/// A [`Recording`] the phone has not announced yet.
async fn two_chunk_recording(seed: u64) -> Recording {
    let chunk_size: i64 = 64 * 1024;
    let test = TestService::with(common::Options {
        chunk_size,
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
    let recording = two_chunk_recording(seed).await;
    let (_, phone, metadata, _) = &recording;
    assert_eq!(phone.announce(metadata).await.status.as_u16(), 201);
    recording
}

/// Runs `request` against the engine on a thread of its own, as a second
/// worker of the listener's runtime would, holds it at its clock read
/// ([`common::WallClock::hold_next_read`]) while `meanwhile` runs, then
/// lets it finish. Returns the held request's answer and what `meanwhile`
/// returned.
async fn held_while<T>(
    test: &TestService,
    request: impl Future<Output = HandoverResponse> + Send + 'static,
    meanwhile: impl Future<Output = T>,
) -> (HandoverResponse, T) {
    let hold = test.clock.hold_next_read();
    let thread = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(request)
    });
    hold.reached();
    let during = meanwhile.await;
    hold.release();
    (thread.join().unwrap(), during)
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
        let (other, chunk) = (phone.clone(), chunks[0].clone());
        async move { other.upload(id, 0, &chunk).await }
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
        let other = phone.clone();
        async move { other.announce(&metadata).await }
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
async fn a_reannounce_during_a_complete_leaves_the_receipt_complete() {
    // The phone announces again (a retry after its own timeout) while its
    // `complete` admits the recording. The announce is held at its clock
    // read, after it read the receipt as `receiving`, while the `complete`
    // runs to the end. Its `receiving` write then finds the receipt
    // `complete` in memory, changes nothing and answers with that. Written
    // back, `receiving` would send the phone's next `complete` back to the
    // start, and the upload after it would become a second meeting.
    let (test, phone, metadata, chunks) = announced_two_chunks(66).await;
    let id = metadata.recording_id;
    for (index, chunk) in chunks.iter().enumerate() {
        let uploaded = phone.upload(id, index as i64, chunk).await;
        assert_eq!(uploaded.status.as_u16(), 204);
    }
    let reannounce = {
        let other = phone.clone();
        async move { other.announce(&metadata).await }
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
        let (other, chunk) = (phone.clone(), chunks[1].clone());
        async move { other.upload(id, 1, &chunk).await }
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
    let (test, phone, metadata, chunks) = two_chunk_recording(68).await;
    let id = metadata.recording_id;
    let second = {
        let (other, metadata) = (phone.clone(), metadata.clone());
        async move { other.announce(&metadata).await }
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
    let (test, phone, metadata, chunks) = two_chunk_recording(69).await;
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

    let (in_memory, stored) = receipts(&test, id);
    assert_eq!(
        (in_memory.device_id, stored.device_id),
        (phone.device.id, phone.device.id)
    );
    assert_eq!(chunk_sets(&test, id), (vec![0], vec![0]));
}
