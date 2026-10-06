//! Every store write, file write and hash is a yield, so another request
//! runs while one awaits. These tests drive the engine directly, so two
//! requests enter it in a known order and the races the loopback clients
//! can only make likely are certain. Two requests on two threads, as on two
//! workers of the listener, meet where one is held at its clock read
//! (`WallClock::hold_next_read`): a receipt change it makes after the read
//! keeps what the other one changed meanwhile.

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
use steno_core::HandoverReceipt;
use steno_core::{AudioFormat, HandoverState, HandoverStateKind};
use steno_handover::engine::{HandoverRequest, Principal, RequestHandling as _};
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

/// A service that does not listen and a phone paired straight into the
/// engine that announced a two-chunk recording, with its chunks.
async fn announced(seed: u64) -> (TestService, EngineDevice, Uuid, Vec<Vec<u8>>) {
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
    assert_eq!(phone.announce(&metadata).await.status.as_u16(), 201);
    (
        test,
        phone,
        metadata.recording_id,
        chunks(&bytes, chunk_size),
    )
}

/// Runs `request` against the engine on a thread of its own, as a second
/// worker of the listener's runtime would.
fn on_another_thread<F>(request: F) -> std::thread::JoinHandle<u16>
where
    F: Future<Output = steno_handover::engine::HandoverResponse> + Send + 'static,
{
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(request)
            .status
            .as_u16()
    })
}

/// The chunk set memory and the store hold for `recording_id`.
fn chunk_sets(test: &TestService, recording_id: Uuid) -> (Vec<i64>, Vec<i64>) {
    let in_memory = test
        .service
        .engine
        .receipts_snapshot()
        .into_iter()
        .find(|receipt| receipt.recording_id == recording_id)
        .map(|receipt: HandoverReceipt| receipt.received_chunks)
        .expect("the receipt is in memory");
    let stored = test
        .store
        .handover_receipt(recording_id)
        .unwrap()
        .expect("the receipt is stored")
        .received_chunks;
    (in_memory, stored)
}

#[tokio::test]
async fn two_chunks_that_land_at_once_both_stay_in_the_receipt() {
    // The phone keeps two chunks in flight, and the listener runs each on
    // a worker of its own. Chunk 0 is held at its clock read, after its
    // file write, while chunk 1 lands in full. Chunk 0 then folds into the
    // receipt as memory holds it, with chunk 1 in it. A fold that read the
    // receipt before the clock and wrote it back after would put back the
    // copy without chunk 1, in memory and in the store.
    let (test, phone, id, chunks) = announced(63).await;
    let hold = test.clock.hold_next_read();
    let first = on_another_thread({
        let other = EngineDevice {
            service: test.service.clone(),
            device: phone.device.clone(),
        };
        let chunk = chunks[0].clone();
        async move { other.upload(id, 0, &chunk).await }
    });
    hold.reached();
    assert_eq!(phone.upload(id, 1, &chunks[1]).await.status.as_u16(), 204);
    hold.release();
    assert_eq!(first.join().unwrap(), 204);

    assert_eq!(chunk_sets(&test, id), (vec![0, 1], vec![0, 1]));
}

#[tokio::test]
async fn a_reannounce_keeps_a_chunk_that_lands_while_it_runs() {
    // The phone announces again (a retry after its own timeout) while its
    // next chunk is in flight. The announce is held at its clock read,
    // after it read the receipt, while chunk 1 lands. Its `receiving`
    // write then changes the receipt as memory holds it, chunk 1 and all.
    let (test, phone, id, chunks) = announced(64).await;
    assert_eq!(phone.upload(id, 0, &chunks[0]).await.status.as_u16(), 204);
    let metadata = test
        .inbox()
        .load_metadata(id)
        .expect("the announce kept its metadata");
    let hold = test.clock.hold_next_read();
    let reannounce = on_another_thread({
        let other = EngineDevice {
            service: test.service.clone(),
            device: phone.device.clone(),
        };
        async move { other.announce(&metadata).await }
    });
    hold.reached();
    assert_eq!(phone.upload(id, 1, &chunks[1]).await.status.as_u16(), 204);
    hold.release();
    assert_eq!(reannounce.join().unwrap(), 200);

    assert_eq!(chunk_sets(&test, id), (vec![0, 1], vec![0, 1]));
}
