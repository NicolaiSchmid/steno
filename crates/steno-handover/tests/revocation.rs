//! Revoking a phone mid-upload: its partial and receipt are gone, every
//! bearer route answers 401 at the gate (the phone's "unpaired" signal),
//! and another phone's upload is untouched. Both ways in: `revoke` from
//! the computer and `DELETE /v1/pairing` from the phone. A request that
//! read its device or receipt before the revoke brings neither back, and a
//! `complete` that had not reached the intake yet admits nothing, also when
//! the phone paired again meanwhile. A recording already admitted answers
//! its meeting. A revoke and a pairing of the same phone commit in the
//! order they were asked for: a revoke during a pairing's save leaves the
//! device revoked, in the store too, and a pairing during a revoke's
//! discards or its delete stays paired. A touch or a receipt save does
//! not commit ahead of a revoke asked for before it. A revoke whose store
//! delete fails still refuses the device, and a partial created again
//! during a verify is never promoted.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

use common::{EngineDevice, Phone, ScriptedIntake, StoreHold, TestService, chunks, seeded_bytes};
use steno_core::{HandoverIntake, HandoverState, RecordingMetadata, Store};
use steno_handover::engine::{
    AuthOutcome, Engine, HandoverResponse, Principal, RequestHandling as _,
};
use steno_handover::pairing::DeviceTokens;
use steno_handover::route::Route;
use steno_handover::{HandoverService, wire};
use tokio::task::{Unconstrained, unconstrained};
use uuid::Uuid;

const CHUNK_SIZE: i64 = 256 * 1024;
/// The chunk size of the tests that drive the engine directly.
const DIRECT_CHUNK_SIZE: i64 = 64 * 1024;

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

/// A service that does not listen, with `intake`, and a phone paired
/// straight into the engine that uploaded every chunk of a recording.
async fn uploaded(
    intake: Option<Arc<dyn HandoverIntake>>,
    seed: u64,
) -> (TestService, EngineDevice, RecordingMetadata, Vec<u8>) {
    let test = TestService::with(common::Options {
        chunk_size: DIRECT_CHUNK_SIZE,
        intake,
        start: false,
        ..common::Options::default()
    })
    .await;
    let phone = EngineDevice::paired(&test, "Direct iPhone").await;
    let bytes = seeded_bytes(2 * DIRECT_CHUNK_SIZE as usize, seed);
    let metadata = phone.metadata(&bytes, DIRECT_CHUNK_SIZE);
    phone.upload_all(&metadata, &bytes).await;
    (test, phone, metadata, bytes)
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
    let intake = ScriptedIntake::gated(Uuid::new_v4(), false);
    let (test, phone, metadata, _) = uploaded(Some(intake.clone()), 97).await;
    let id = metadata.recording_id;
    let receipts = test.service.receipts();

    let completing = phone.complete(id);
    let revoking = async {
        intake.admitting().await;
        test.service.revoke(phone.device.id).await.unwrap();
        intake.release();
    };
    let (_, ()) = tokio::join!(completing, revoking);
    assert_eq!(intake.count(), 1, "the intake admitted once");

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
    let again = phone.pair_again().await;
    let bytes = seeded_bytes(DIRECT_CHUNK_SIZE as usize, 98);
    let metadata = again.metadata(&bytes, DIRECT_CHUNK_SIZE);
    assert_eq!(again.announce(&metadata).await.status.as_u16(), 201);
    assert!(owned(&test.service.engine.receipts_snapshot()));
    assert!(owned(&receipts.borrow()));
}

/// The computer comes back over the store and inbox of a phone that
/// uploaded every chunk: after a restart its receipt is only in the store.
struct Restarted {
    first: TestService,
    metadata: RecordingMetadata,
    bytes: Vec<u8>,
    service: Arc<HandoverService>,
    intake: Arc<ScriptedIntake>,
    /// The phone's view of the restarted engine.
    phone: EngineDevice,
}

impl Restarted {
    async fn new() -> Restarted {
        let (first, before, metadata, bytes) = uploaded(None, 99).await;
        let intake = ScriptedIntake::new(Uuid::new_v4(), 0);
        let service = Arc::new(HandoverService::new(
            first.service.configuration.clone(),
            first.store.clone(),
            common::taking(intake.clone()),
            first.service.identity.clone(),
            first.clock.clock(),
        ));
        let phone = EngineDevice {
            service: service.clone(),
            device: before.device,
        };
        Restarted {
            first,
            metadata,
            bytes,
            service,
            intake,
            phone,
        }
    }

    fn id(&self) -> Uuid {
        self.metadata.recording_id
    }

    /// Nothing reached the intake, and nothing of the upload is left in
    /// memory or the store.
    fn assert_nothing_admitted(&self) {
        assert_eq!(self.intake.count(), 0, "the intake never sees the file");
        assert!(self.service.engine.receipts_snapshot().is_empty());
        assert_eq!(self.first.store.handover_receipt(self.id()).unwrap(), None);
    }

    /// The upload's files are gone from the inbox.
    fn files_are_gone(&self) -> bool {
        let inbox = self.first.inbox();
        let id = self.id();
        !inbox.has_partial(id)
            && !inbox.has_verified(id, self.metadata.format)
            && inbox.load_metadata(id).is_none()
    }

    /// `complete` on the restarted engine with a revoke that lands while it
    /// reads the receipt from the store. With `pairs_again`, the phone pairs
    /// again under the same device id before `complete` goes on; the new
    /// pairing's view comes back with the answer.
    async fn complete_with_a_revoke_during_the_read(
        &self,
        pairs_again: bool,
    ) -> (HandoverResponse, Option<EngineDevice>) {
        let (completing, again) = self.revoke_during_the_read(pairs_again).await;
        (completing.await, again)
    }

    /// Runs `complete` to just after its store read
    /// ([`past_the_store_read`]), then revokes the device (and pairs it
    /// again with `pairs_again`), and hands back the `complete`, not polled
    /// since its read returned. An await added before the read would leave
    /// it for after the revoke, and the tests fail with a 404 (the row is
    /// gone) instead of a 401.
    async fn revoke_during_the_read(
        &self,
        pairs_again: bool,
    ) -> (
        Pin<Box<impl Future<Output = HandoverResponse> + '_>>,
        Option<EngineDevice>,
    ) {
        let completing =
            past_the_store_read(&self.first.store, self.phone.complete(self.id())).await;
        self.service.revoke(self.phone.device.id).await.unwrap();
        let again = if pairs_again {
            Some(self.phone.pair_again().await)
        } else {
            None
        };
        (completing, again)
    }
}

/// Runs `request` to its first store read and waits there until the read
/// has returned, then hands it back, not polled since. The store is held,
/// so the read cannot finish before the first poll returns; this takes the
/// store read to be the first point where `request` waits. The future runs
/// unconstrained, so a wake means the read returned, never that tokio's
/// budget ran out.
async fn past_the_store_read<F: Future>(
    store: &Arc<Store>,
    request: F,
) -> Pin<Box<Unconstrained<F>>> {
    let hold = StoreHold::new(store);
    let woken = Woken::new();
    let mut request = Box::pin(unconstrained(request));
    woken.pending(request.as_mut(), "the request waits on the store read");
    hold.release();
    woken.wait("the store read returns").await;
    request
}

/// A revoke that lands while `complete` reads the store finds nothing in
/// memory to discard, so the files are still there for the verify; the
/// engine itself must keep a revoked device's recording from the intake.
async fn a_revoke_during_the_read_admits_nothing(pairs_again: bool) {
    let restarted = Restarted::new().await;
    let (response, again) = restarted
        .complete_with_a_revoke_during_the_read(pairs_again)
        .await;

    assert_eq!(
        response.status.as_u16(),
        401,
        "the phone learns it is unpaired (a 404: `complete` did not wait in its receipt read)"
    );
    restarted.assert_nothing_admitted();
    assert!(restarted.files_are_gone(), "its files are gone");
    if let Some(again) = again {
        // The new pairing uploads the recording again, and it goes through.
        again
            .upload_all(&restarted.metadata, &restarted.bytes)
            .await;
        assert_eq!(again.complete(restarted.id()).await.status.as_u16(), 200);
        assert_eq!(restarted.intake.count(), 1);
    }
}

#[tokio::test]
async fn a_complete_that_read_its_receipt_before_a_revoke_admits_nothing() {
    a_revoke_during_the_read_admits_nothing(false).await;
}

#[tokio::test]
async fn a_phone_that_paired_again_does_not_let_the_old_complete_through() {
    a_revoke_during_the_read_admits_nothing(true).await;
}

#[tokio::test]
async fn an_admitted_recording_answers_its_meeting_after_a_revoke_during_the_read() {
    // The recording was admitted before the restart; the phone lost the
    // answer and asks again. A 401 would make it keep the recording, and
    // its upload after pairing again would become a second meeting. The
    // phone pairs again before `complete` goes on, so the read puts the
    // receipt back into memory; the receipt is gone from the store, so it
    // must not stay in the stream either.
    let restarted = Restarted::new().await;
    let before = EngineDevice {
        service: restarted.first.service.clone(),
        device: restarted.phone.device.clone(),
    };
    let admitted = before.complete(restarted.id()).await;
    assert_eq!(admitted.status.as_u16(), 200);
    let meeting: wire::CompleteResponse = admitted.decode().unwrap();

    let (response, _) = restarted.complete_with_a_revoke_during_the_read(true).await;

    assert_eq!(
        response.status.as_u16(),
        200,
        "the phone keeps its meeting (a 404: the store read must be the first wait)"
    );
    let answered: wire::CompleteResponse = response.decode().unwrap();
    assert_eq!(answered.meeting_id, meeting.meeting_id);
    restarted.assert_nothing_admitted();
}

#[tokio::test]
async fn a_complete_during_a_revokes_store_delete_admits_nothing() {
    // A `complete` that starts while the revoke's store delete is pending
    // takes the count the revoke already bumped, and its read may still be
    // served before the delete. It is refused before it reads: with the
    // store held, any answer on the first poll comes from the entry check.
    let restarted = Restarted::new().await;
    let hold = StoreHold::new(&restarted.first.store);
    let woken = Woken::new();
    let mut revoking = pin!(restarted.service.revoke(restarted.phone.device.id));
    woken.pending(revoking.as_mut(), "the revoke waits on its store delete");
    let mut completing = pin!(restarted.phone.complete(restarted.id()));
    let Poll::Ready(response) = woken.poll(completing.as_mut()) else {
        panic!("complete is refused before it reads the store");
    };
    assert_eq!(response.status.as_u16(), 401);
    hold.release();
    revoking.await.unwrap();

    restarted.assert_nothing_admitted();
    // Neither the refused `complete` nor the revoke knew the receipt; the
    // next start's sweep takes its files.
    restarted.service.engine.sweep_orphans().await;
    assert!(restarted.files_are_gone(), "the sweep takes the files");
}

#[tokio::test]
async fn a_revoke_during_the_verify_leaves_the_new_pairings_upload_alone() {
    // The revoke lands while `complete` writes `verifying`; it finds the
    // receipt in memory and discards the files. The phone pairs again and
    // announces anew before the old `complete` goes on, which creates the
    // partial again. The old `complete` must neither discard that partial,
    // write `failed` over the new receipt nor promote it.
    let intake = ScriptedIntake::new(Uuid::new_v4(), 0);
    let (test, phone, metadata, bytes) = uploaded(Some(intake.clone()), 96).await;
    let id = metadata.recording_id;
    let inbox = test.inbox();

    // `complete` waits on its `verifying` write, which the held store keeps
    // from returning.
    let hold = StoreHold::new(&test.store);
    let saved = Woken::new();
    let mut completing = pin!(unconstrained(phone.complete(id)));
    saved.pending(completing.as_mut(), "complete waits on its verifying write");
    let mut revoking = pin!(test.service.revoke(phone.device.id));
    Woken::new().pending(revoking.as_mut(), "the revoke waits on its store delete");
    assert!(!inbox.has_partial(id), "the revoke discarded the partial");
    hold.release();
    // The delete waits in line behind the `verifying` write, which returns
    // on its own: the old `complete` is not polled until the end.
    common::signalled("the revoke returns", revoking)
        .await
        .unwrap();
    // The delete ran after the `verifying` write, so no row is left; a
    // write after the pairing's save would put the row back for the new
    // pairing.
    saved.wait("the verifying write returns").await;
    let again = phone.pair_again().await;

    // The new announce creates the partial, then waits on its receipt save.
    let announced = Woken::new();
    let mut announcing = pin!(again.announce(&metadata));
    loop {
        announced.pending(announcing.as_mut(), "the announce waits on its store calls");
        if inbox.has_partial(id) {
            break;
        }
        announced.wait("the announce's store read returns").await;
    }
    let (old, announce) = tokio::join!(completing, announcing);

    assert_eq!(old.status.as_u16(), 401, "the old complete is refused");
    assert_eq!(announce.status.as_u16(), 201);
    assert!(inbox.has_partial(id), "the new partial stays");
    let receipts = test.service.engine.receipts_snapshot();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].state,
        steno_core::HandoverState::Receiving,
        "the new receipt is not failed"
    );
    again.upload_all(&metadata, &bytes).await;
    assert_eq!(again.complete(id).await.status.as_u16(), 200);
    assert_eq!(intake.count(), 1);
}

#[tokio::test]
async fn a_revoke_during_the_verify_leaves_another_phones_upload_alone() {
    // The revoke lands while `complete` writes `verifying`; it finds the
    // receipt in memory and discards the files. Another phone announces the
    // same recording id before the old `complete` goes on, which opens a
    // partial and a sidecar. The old `complete` then sees the revoke after
    // its hash while `revoked` still holds the device, and the files there
    // are not the revoked phone's: memory holds the other phone's receipt,
    // so they stay.
    let intake = ScriptedIntake::new(Uuid::new_v4(), 0);
    let (test, phone, metadata, bytes) = uploaded(Some(intake.clone()), 97).await;
    let id = metadata.recording_id;
    let inbox = test.inbox();

    let hold = StoreHold::new(&test.store);
    let saved = Woken::new();
    let mut completing = pin!(unconstrained(phone.complete(id)));
    saved.pending(completing.as_mut(), "complete waits on its verifying write");
    let mut revoking = pin!(test.service.revoke(phone.device.id));
    Woken::new().pending(revoking.as_mut(), "the revoke waits on its store delete");
    hold.release();
    common::signalled("the revoke returns", revoking)
        .await
        .unwrap();
    saved.wait("the verifying write returns").await;
    let other = EngineDevice::paired(&test, "Other iPhone").await;
    assert_eq!(other.announce(&metadata).await.status.as_u16(), 201);

    let old = common::signalled("the old complete answers", completing).await;
    assert_eq!(old.status.as_u16(), 401, "the old complete is refused");
    assert!(inbox.has_partial(id), "the other phone's partial stays");
    assert!(inbox.load_metadata(id).is_some(), "and so does its sidecar");
    let receipts = test.service.engine.receipts_snapshot();
    assert_eq!(
        receipts.iter().map(|r| r.device_id).collect::<Vec<_>>(),
        vec![other.device.id]
    );
    other.upload_all(&metadata, &bytes).await;
    assert_eq!(other.complete(id).await.status.as_u16(), 200);
    assert_eq!(intake.count(), 1);
}

#[tokio::test]
async fn a_refused_complete_leaves_another_phones_announce_alone() {
    // After a restart the receipt is only in the store, and a revoke lands
    // while `complete` reads it. Another phone announces the same recording
    // id, and its announce is held at its clock read, after its receipt
    // read found nothing, while the old `complete` goes on and is refused:
    // the refusal discards the files the revoke missed and forgets the
    // receipt. The announce makes its receipt and opens its files only
    // after that, so both stay.
    let restarted = Restarted::new().await;
    let id = restarted.id();
    let (completing, _) = restarted.revoke_during_the_read(false).await;
    let other =
        EngineDevice::paired_on(&restarted.service, &restarted.first.store, "Other iPhone").await;
    let announce = {
        let (other, metadata) = (other.clone(), restarted.metadata.clone());
        async move { other.announce(&metadata).await }
    };
    let (announced, refused) = common::held_while(&restarted.first, announce, completing).await;

    assert_eq!(refused.status.as_u16(), 401, "the old complete is refused");
    assert_eq!(announced.status.as_u16(), 201);
    let inbox = restarted.first.inbox();
    assert!(inbox.has_partial(id), "the other phone's partial stays");
    assert!(inbox.load_metadata(id).is_some(), "and so does its sidecar");
    let receipts = restarted.service.engine.receipts_snapshot();
    assert_eq!(
        receipts.iter().map(|r| r.device_id).collect::<Vec<_>>(),
        vec![other.device.id]
    );
    other
        .upload_all(&restarted.metadata, &restarted.bytes)
        .await;
    assert_eq!(other.complete(id).await.status.as_u16(), 200);
    assert_eq!(restarted.intake.count(), 1);
}

#[tokio::test]
async fn a_reannounce_of_a_phone_revoked_during_its_read_opens_no_files() {
    // After a restart the receipt is only in the store, and its files are
    // gone (a sweep). A revoke lands while the phone's re-announce reads the
    // receipt, so the receipt stays out of memory. The re-announce is
    // refused before it opens files: files with no receipt in memory would
    // be no device's, and another phone's discard would take them.
    let restarted = Restarted::new().await;
    let id = restarted.id();
    restarted.first.inbox().discard(id);
    let announcing = past_the_store_read(
        &restarted.first.store,
        restarted.phone.announce(&restarted.metadata),
    )
    .await;
    restarted
        .service
        .revoke(restarted.phone.device.id)
        .await
        .unwrap();

    assert_eq!(announcing.await.status.as_u16(), 401);
    assert!(restarted.files_are_gone(), "no file is opened");
    assert!(restarted.service.engine.receipts_snapshot().is_empty());
}

#[tokio::test]
async fn a_first_announce_of_a_phone_revoked_during_its_read_opens_no_files() {
    // A revoke whose store delete fails lands while the phone's first
    // announce of a recording reads the store: the device stays in the
    // store, so the receipt's save goes through, but memory keeps it out.
    // The announce opens no files and is answered 401.
    let test = TestService::with(common::Options {
        chunk_size: DIRECT_CHUNK_SIZE,
        start: false,
        ..common::Options::default()
    })
    .await;
    let phone = EngineDevice::paired(&test, "Direct iPhone").await;
    let bytes = seeded_bytes(DIRECT_CHUNK_SIZE as usize, 96);
    let metadata = phone.metadata(&bytes, DIRECT_CHUNK_SIZE);
    let id = metadata.recording_id;
    common::execute_batch(
        &test.store,
        "CREATE TEMP TRIGGER refuse_revoke BEFORE DELETE ON pairedDevice \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    );
    let announcing = past_the_store_read(&test.store, phone.announce(&metadata)).await;
    assert!(test.service.revoke(phone.device.id).await.is_err());

    assert_eq!(announcing.await.status.as_u16(), 401);
    let inbox = test.inbox();
    assert!(!inbox.has_partial(id), "no partial is opened");
    assert!(inbox.load_metadata(id).is_none(), "and no sidecar");
    assert!(test.service.engine.receipts_snapshot().is_empty());
}

/// What the bearer gate of the computer, started again over `test`'s
/// store, makes of `token`. Memory starts empty, so only the store decides.
async fn gate_after_a_restart(test: &TestService, token: &str) -> AuthOutcome {
    let restarted = HandoverService::new(
        test.service.configuration.clone(),
        test.store.clone(),
        test.intake.clone(),
        test.service.identity.clone(),
        test.clock.clock(),
    );
    restarted
        .engine
        .authenticate(Route::Status(Uuid::new_v4()), Some(&common::bearer(token)))
        .await
}

/// A service that does not listen, a phone paired straight into the engine
/// and a second pairing window with the gate's answer for its secret.
async fn paired_with_a_window_open() -> (TestService, EngineDevice, Principal) {
    let test = TestService::with(common::Options {
        start: false,
        ..common::Options::default()
    })
    .await;
    let phone = EngineDevice::paired(&test, "Direct iPhone").await;
    let payload = test.service.begin_pairing();
    let principal = common::pairing_principal(&test.service, &payload).await;
    (test, phone, principal)
}

/// `first` and `second` run to the end together, bounded by
/// [`common::signalled`]; `what` names them.
async fn both<A: Future, B: Future>(what: &str, first: A, second: B) -> (A::Output, B::Output) {
    common::signalled(what, async { tokio::join!(first, second) }).await
}

#[test]
fn a_revoke_during_a_pairings_save_keeps_the_device_revoked() {
    // The phone pairs again while the computer revokes it. The revoke
    // started after the pairing read the count, and its delete is asked
    // for after the save, so it commits after it: the device stays
    // revoked until the next pairing, in memory and in the store, so a
    // restart does not let it back in.
    common::on_one_worker(async {
        let (test, phone, principal) = paired_with_a_window_open().await;
        let woken = Woken::new();
        let mut pairing = pin!(common::engine_pair_as(
            &test.service,
            principal,
            phone.device.id,
            &phone.device.name,
        ));
        woken.pending(pairing.as_mut(), "the pairing waits on its save");
        let mut revoking = pin!(test.service.revoke(phone.device.id));
        woken.pending(revoking.as_mut(), "the revoke waits on its store delete");
        let (paired, revoked) = both("the pairing and the revoke", pairing, revoking).await;
        assert_eq!(paired.status.as_u16(), 200);
        revoked.unwrap();
        let token = paired.decode::<wire::PairResponse>().unwrap().token;

        assert_eq!(
            test.store.paired_device(phone.device.id).unwrap(),
            None,
            "the delete commits after the save"
        );
        assert_eq!(
            gate_after_a_restart(&test, &token).await,
            AuthOutcome::Rejected(Engine::unauthorized()),
            "a restart does not let the revoked phone back in"
        );
        let unknown = Uuid::new_v4();
        assert_eq!(
            phone.complete(unknown).await.status.as_u16(),
            401,
            "the device is still revoked"
        );
        let again = phone.pair_again().await;
        assert_eq!(
            again.complete(unknown).await.status.as_u16(),
            404,
            "the next pairing clears it"
        );
    });
}

#[test]
fn a_pairing_during_a_revokes_delete_stays_paired() {
    // The computer revokes the phone, and the phone, told so, pairs again
    // before the delete has returned. The pairing read the count after the
    // revoke's bump, and its save is asked for after the delete, so it
    // commits after it: the new pairing stays, in the store and after a
    // restart, and its recording routes answer instead of a 401 that
    // would make the phone unpair itself.
    common::on_one_worker(async {
        let (test, phone, principal) = paired_with_a_window_open().await;
        let woken = Woken::new();
        let mut revoking = pin!(test.service.revoke(phone.device.id));
        woken.pending(revoking.as_mut(), "the revoke waits on its store delete");
        let mut pairing = pin!(common::engine_pair_as(
            &test.service,
            principal,
            phone.device.id,
            &phone.device.name,
        ));
        woken.pending(pairing.as_mut(), "the pairing waits on its save");
        let (revoked, paired) = both("the revoke and the pairing", revoking, pairing).await;
        revoked.unwrap();
        assert_eq!(paired.status.as_u16(), 200);
        let token = paired.decode::<wire::PairResponse>().unwrap().token;

        let device = test
            .store
            .paired_device_for_token_hash(&DeviceTokens::hash(&token))
            .unwrap()
            .expect("the save commits after the delete");
        assert_eq!(device.id, phone.device.id);
        assert!(matches!(
            gate_after_a_restart(&test, &token).await,
            AuthOutcome::Allowed(Principal::Device(_))
        ));
        let again = EngineDevice {
            service: test.service.clone(),
            device,
        };
        assert_eq!(
            again.status(Uuid::new_v4()).await.status.as_u16(),
            404,
            "the new pairing is not revoked"
        );
    });
}

#[test]
fn a_touch_does_not_overtake_a_revoke_asked_for_before_it() {
    // The gate refreshes `last_seen_at` of a device whose revoke is already
    // on its way to the store. The touch takes its place in line behind the
    // delete: a read asked for after both, which skips the line, finds
    // neither committed, where a touch sent straight to the pool would have
    // committed ahead of it. That the touch then waits for the delete is
    // the order tests' part: the read reaches the pool before either task
    // runs, so a line that does not wait passes here too. The read in
    // between holds only on `on_one_worker`, where neither task runs before
    // this one yields.
    common::on_one_worker(async {
        let test = TestService::with(common::Options {
            start: false,
            ..common::Options::default()
        })
        .await;
        let payload = test.service.begin_pairing();
        let device_id = Uuid::new_v4();
        let paired = common::engine_pair(&test.service, &payload, device_id, "Direct iPhone").await;
        let token = paired.decode::<wire::PairResponse>().unwrap().token;
        let before = test.store.paired_device(device_id).unwrap().unwrap();
        test.advance(Duration::from_secs(
            Engine::LAST_SEEN_RESOLUTION_SECONDS as u64 + 1,
        ));

        let woken = Woken::new();
        let mut revoking = pin!(test.service.revoke(device_id));
        woken.pending(revoking.as_mut(), "the revoke waits on its store delete");
        let mut touching = pin!(
            test.service
                .engine
                .touch(before.clone(), DeviceTokens::hash(&token))
        );
        woken.pending(touching.as_mut(), "the touch waits on the delete");
        assert_eq!(
            test.service.paired_devices().await.unwrap(),
            vec![before],
            "nothing reached the store ahead of the delete"
        );
        let (revoked, _) = both("the revoke and the touch", revoking, touching).await;
        revoked.unwrap();
        assert_eq!(test.service.paired_devices().await.unwrap(), Vec::new());
    });
}

#[test]
fn a_receipt_save_does_not_overtake_a_revoke_asked_for_before_it() {
    // The computer revokes one phone while another announces its recording
    // again. The announce finds the receipt in memory and goes to its save
    // without a yield, so the save takes its place in line behind the
    // delete: a read asked for after both, which skips the line, finds
    // neither committed, where a save sent straight to the pool would have
    // committed ahead of it. That the save then waits for the delete is the
    // order tests' part. The task count and the read in between hold only
    // on `on_one_worker`, where neither task runs before this one yields.
    common::on_one_worker(async {
        let test = TestService::with(common::Options {
            chunk_size: DIRECT_CHUNK_SIZE,
            start: false,
            ..common::Options::default()
        })
        .await;
        let revoked = EngineDevice::paired(&test, "Direct iPhone").await;
        let other = EngineDevice::paired(&test, "Other iPhone").await;
        let bytes = seeded_bytes(2 * DIRECT_CHUNK_SIZE as usize, 101);
        let metadata = other.metadata(&bytes, DIRECT_CHUNK_SIZE);
        let id = metadata.recording_id;
        assert_eq!(other.announce(&metadata).await.status.as_u16(), 201);
        let before = test.store.handover_receipt(id).unwrap();
        assert!(before.is_some());
        test.advance(Duration::from_secs(1));

        let woken = Woken::new();
        let mut revoking = pin!(test.service.revoke(revoked.device.id));
        woken.pending(revoking.as_mut(), "the revoke waits on its store delete");
        let mut announcing = pin!(other.announce(&metadata));
        woken.pending(
            announcing.as_mut(),
            "the announce waits on its receipt save",
        );
        assert_eq!(
            tokio::runtime::Handle::current()
                .metrics()
                .num_alive_tasks(),
            3,
            "the delete and the save wait in their own tasks"
        );
        // One read on the pool, asked for before either task ran: the one
        // blocking thread runs it ahead of the delete and the save, and
        // behind a save sent straight to the pool.
        let store = test.store.clone();
        let (devices, receipt) = tokio::task::spawn_blocking(move || {
            (
                store.paired_devices().unwrap().len(),
                store.handover_receipt(id).unwrap(),
            )
        })
        .await
        .unwrap();
        assert_eq!(devices, 2, "the delete has not committed");
        assert_eq!(
            receipt, before,
            "nothing reached the store ahead of the delete"
        );
        let (deleted, announced) = both("the revoke and the announce", revoking, announcing).await;
        deleted.unwrap();
        assert_eq!(announced.status.as_u16(), 200);
        assert_ne!(
            test.store.handover_receipt(id).unwrap(),
            before,
            "the save commits"
        );
    });
}

#[test]
fn a_pairing_during_a_revokes_discards_stays_paired() {
    // The phone pairs again while the computer's revoke, past its count
    // bump, discards the phone's unfinished uploads. The revoke took the
    // delete's place in line under the same guard as the bump, so the
    // pairing, which reads the bumped count, saves after the delete and
    // stays. A place taken after the discards would let the save commit
    // first and the delete remove the new pairing. The discards do not
    // yield, so on `on_one_worker` nothing could run among them. Here the
    // pairing spins on a second worker until the first partial is gone, so
    // it usually lands among the discards. It always starts after the
    // bump, so the test cannot fail on the fixed code however the threads
    // run.
    const UNFINISHED: u64 = 200;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (test, phone, principal) = paired_with_a_window_open().await;
            let mut ids = Vec::new();
            for seed in 0..UNFINISHED {
                let bytes = seeded_bytes(DIRECT_CHUNK_SIZE as usize, seed);
                let metadata = phone.metadata(&bytes, DIRECT_CHUNK_SIZE);
                assert_eq!(phone.announce(&metadata).await.status.as_u16(), 201);
                ids.push(metadata.recording_id);
            }
            // The revoke discards in recording id order.
            let first = ids.into_iter().min().unwrap();
            assert!(
                test.service.engine.inbox.has_partial(first),
                "the first upload has a partial before the revoke, so the pairing waits for its discard"
            );
            let service = test.service.clone();
            let (device_id, name) = (phone.device.id, phone.device.name.clone());
            let (started, waiting) = tokio::sync::oneshot::channel();
            let pairing = tokio::spawn(async move {
                started.send(()).unwrap();
                let deadline = Instant::now() + common::SIGNAL_BOUND;
                while service.engine.inbox.has_partial(first) {
                    assert!(
                        Instant::now() < deadline,
                        "the revoke discards within {:?}",
                        common::SIGNAL_BOUND
                    );
                    std::hint::spin_loop();
                }
                common::engine_pair_as(&service, principal, device_id, &name).await
            });
            common::signalled("the pairing waits on the other worker", waiting)
                .await
                .unwrap();
            let (revoked, paired) = both(
                "the revoke and the pairing",
                test.service.revoke(device_id),
                pairing,
            )
            .await;
            revoked.unwrap();
            let paired = paired.unwrap();
            assert_eq!(paired.status.as_u16(), 200);
            let token = paired.decode::<wire::PairResponse>().unwrap().token;
            assert!(
                test.store
                    .paired_device_for_token_hash(&DeviceTokens::hash(&token))
                    .unwrap()
                    .is_some(),
                "the save commits after the delete"
            );
        });
}

#[tokio::test]
async fn a_partial_created_again_during_the_verify_is_not_promoted() {
    // An old `complete` waits right after its store read while the phone
    // is revoked, pairs again, uploads anew and completes. While the new
    // `complete` writes `verifying`, the old one goes on: refused, it
    // discards the new partial. The phone's retried announce creates the
    // partial again, empty. The new `complete` hashes the file it opened,
    // the discarded one, and must not promote the empty partial in its
    // place: the intake would admit an empty file and the phone delete its
    // recording.
    let restarted = Restarted::new().await;
    let id = restarted.id();
    let inbox = restarted.first.inbox();
    let (mut stale, again) = restarted.revoke_during_the_read(true).await;
    let again = again.expect("paired again");
    again
        .upload_all(&restarted.metadata, &restarted.bytes)
        .await;

    let hold = StoreHold::new(&restarted.first.store);
    let mut completing = pin!(unconstrained(again.complete(id)));
    Woken::new().pending(
        completing.as_mut(),
        "the new complete waits on its verifying write",
    );
    assert_eq!(
        restarted.service.engine.receipts_snapshot()[0].state,
        HandoverState::Verifying,
        "the new complete is verifying, with the partial open"
    );
    let Poll::Ready(refused) = Woken::new().poll(stale.as_mut()) else {
        panic!(
            "the old complete answers without the store (the store read must be the first wait)"
        );
    };
    assert_eq!(refused.status.as_u16(), 401);
    assert!(!inbox.has_partial(id), "the refusal discarded the partial");

    // The retried announce reads the receipt the refusal forgot from the
    // store, creates the partial, then waits on its receipt save, in line
    // behind the new `complete`'s `verifying` write.
    let announced = Woken::new();
    let mut announcing = pin!(unconstrained(again.announce(&restarted.metadata)));
    announced.pending(announcing.as_mut(), "the announce waits on its store read");
    hold.release();
    while !inbox.has_partial(id) {
        announced.wait("the announce's store read returns").await;
        announced.pending(
            announcing.as_mut(),
            "the announce waits on its receipt save",
        );
    }
    let (completed, announce) = tokio::join!(completing, announcing);

    assert_eq!(completed.status.as_u16(), 409, "not admitted");
    let status: wire::RecordingStatus = completed.decode().unwrap();
    assert_eq!(
        status.received_chunks,
        Vec::<i64>::new(),
        "every chunk again"
    );
    assert_eq!(announce.status.as_u16(), 200);
    assert_eq!(restarted.intake.count(), 0, "the intake never sees a file");
    assert!(inbox.has_partial(id), "the partial created again stays");
    again
        .upload_all(&restarted.metadata, &restarted.bytes)
        .await;
    assert_eq!(again.complete(id).await.status.as_u16(), 200);
    let admitted = restarted.intake.entries();
    assert_eq!(admitted.len(), 1);
    assert_eq!(std::fs::read(&admitted[0]).unwrap(), restarted.bytes);
}

#[tokio::test]
async fn a_partial_gone_during_the_verify_lists_no_chunk() {
    // As above, without the retried announce: the refusal discarded the
    // partial the new `complete` hashes, and nothing is at its path when the
    // hash returns. The 409 lists no chunk, so the phone announces and sends
    // every chunk again.
    let restarted = Restarted::new().await;
    let id = restarted.id();
    let inbox = restarted.first.inbox();
    let (mut stale, again) = restarted.revoke_during_the_read(true).await;
    let again = again.expect("paired again");
    again
        .upload_all(&restarted.metadata, &restarted.bytes)
        .await;

    let hold = StoreHold::new(&restarted.first.store);
    let mut completing = pin!(unconstrained(again.complete(id)));
    Woken::new().pending(
        completing.as_mut(),
        "the new complete waits on its verifying write",
    );
    let Poll::Ready(refused) = Woken::new().poll(stale.as_mut()) else {
        panic!(
            "the old complete answers without the store (the store read must be the first wait)"
        );
    };
    assert_eq!(refused.status.as_u16(), 401);
    assert!(!inbox.has_partial(id), "the refusal discarded the partial");
    hold.release();
    let completed = completing.await;

    assert_eq!(completed.status.as_u16(), 409, "not admitted");
    let status: wire::RecordingStatus = completed.decode().unwrap();
    assert_eq!(
        status.received_chunks,
        Vec::<i64>::new(),
        "every chunk again"
    );
    assert_eq!(restarted.intake.count(), 0, "the intake never sees a file");
    again
        .upload_all(&restarted.metadata, &restarted.bytes)
        .await;
    assert_eq!(again.complete(id).await.status.as_u16(), 200);
    assert_eq!(restarted.intake.count(), 1);
}

#[tokio::test]
async fn a_revoked_phones_partial_created_during_the_verify_is_discarded() {
    // The revoke lands while `complete` writes `verifying` and discards the
    // files. An announce of the revoked phone that passed the gate before
    // the revoke creates the partial again (here straight through the
    // inbox, as its `Inbox::begin` does). No pairing followed, so the
    // partial can only be the revoked phone's: the refused `complete`
    // discards it.
    let intake = ScriptedIntake::new(Uuid::new_v4(), 0);
    let (test, phone, metadata, _) = uploaded(Some(intake.clone()), 94).await;
    let id = metadata.recording_id;
    let inbox = test.inbox();

    let hold = StoreHold::new(&test.store);
    let mut completing = pin!(unconstrained(phone.complete(id)));
    Woken::new().pending(completing.as_mut(), "complete waits on its verifying write");
    let mut revoking = pin!(test.service.revoke(phone.device.id));
    Woken::new().pending(revoking.as_mut(), "the revoke waits on its store delete");
    assert!(!inbox.has_partial(id), "the revoke discarded the partial");
    inbox.begin(&metadata).unwrap();
    hold.release();
    let (refused, revoked) = tokio::join!(completing, revoking);

    assert_eq!(refused.status.as_u16(), 401);
    revoked.unwrap();
    assert!(!inbox.has_partial(id), "the partial created again is gone");
    assert!(inbox.load_metadata(id).is_none());
    assert_eq!(intake.count(), 0);
}

#[tokio::test]
async fn a_failed_revoke_delete_publishes_the_receipts_and_refuses_the_device() {
    // The store refuses the delete. The device stays in the store, so its
    // token still passes the gate; in memory it is revoked: its receipt
    // leaves the stream and every recording route answers 401, until it
    // pairs again.
    let (test, phone, metadata, bytes) = uploaded(None, 95).await;
    let id = metadata.recording_id;
    let receipts = test.service.receipts();
    assert_eq!(receipts.borrow().len(), 1);
    common::execute_batch(
        &test.store,
        "CREATE TEMP TRIGGER refuse_revoke BEFORE DELETE ON pairedDevice \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    );

    assert!(test.service.revoke(phone.device.id).await.is_err());
    assert!(
        receipts
            .borrow()
            .iter()
            .all(|receipt| receipt.recording_id != id),
        "the stream drops the receipt"
    );
    assert!(
        test.store.paired_device(phone.device.id).unwrap().is_some(),
        "the device is still in the store"
    );
    assert_eq!(phone.announce(&metadata).await.status.as_u16(), 401);
    assert_eq!(phone.status(id).await.status.as_u16(), 401);
    assert_eq!(
        phone
            .upload(id, 0, &chunks(&bytes, DIRECT_CHUNK_SIZE)[0])
            .await
            .status
            .as_u16(),
        401
    );
    assert_eq!(phone.complete(id).await.status.as_u16(), 401);
    assert_eq!(test.intake.admissions.count(), 0);

    common::execute_batch(&test.store, "DROP TRIGGER temp.refuse_revoke");
    let again = phone.pair_again().await;
    again.upload_all(&metadata, &bytes).await;
    assert_eq!(again.complete(id).await.status.as_u16(), 200);
}

/// The waker of a future the test polls by hand.
struct Woken(tokio::sync::Notify);

impl Woken {
    fn new() -> Arc<Woken> {
        Arc::new(Woken(tokio::sync::Notify::new()))
    }

    /// Polls `future` once with this waker.
    fn poll<F: Future>(self: &Arc<Self>, future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(&Waker::from(self.clone())))
    }

    /// Polls `future` once with this waker and asserts it is pending;
    /// `what` names what it waits on.
    #[track_caller]
    fn pending<F: Future>(self: &Arc<Self>, future: Pin<&mut F>, what: &str) {
        assert!(self.poll(future).is_pending(), "{what}");
    }

    /// Returns once a future polled with this waker can go on.
    async fn wait(&self, what: &str) {
        common::signalled(what, self.0.notified()).await;
    }
}

impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }
}
