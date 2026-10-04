//! Revoking a phone mid-upload: its partial and receipt are gone, every
//! bearer route answers 401 at the gate (the phone's "unpaired" signal),
//! and another phone's upload is untouched. Both ways in: `revoke` from
//! the computer and `DELETE /v1/pairing` from the phone. A request that
//! read its device or receipt before the revoke brings neither back, and a
//! `complete` that had not reached the intake yet admits nothing, also when
//! the phone paired again meanwhile.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::pin::Pin;
use std::sync::{Arc, mpsc};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use common::{Phone, TestService, chunks, seeded_bytes};
use steno_core::{RecordingMetadata, Store};
use steno_handover::engine::HandoverResponse;
use steno_handover::pairing::DeviceTokens;
use steno_handover::{HandoverService, wire};
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
    let bytes = seeded_bytes(chunk_size as usize, 98);
    let metadata = again.metadata(&bytes, chunk_size);
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
    intake: Arc<common::ScriptedIntake>,
    /// The phone's view of the restarted engine.
    phone: common::EngineDevice,
}

impl Restarted {
    async fn new() -> Restarted {
        let chunk_size: i64 = 64 * 1024;
        let first = TestService::with(common::Options {
            chunk_size,
            start: false,
            ..common::Options::default()
        })
        .await;
        let before = common::EngineDevice::paired(&first, "Direct iPhone").await;
        let bytes = seeded_bytes(2 * chunk_size as usize, 99);
        let metadata = before.metadata(&bytes, chunk_size);
        before.upload_all(&metadata, &bytes).await;
        let intake = common::ScriptedIntake::new(Uuid::new_v4(), 0);
        let service = Arc::new(HandoverService::new(
            first.service.configuration.clone(),
            first.store.clone(),
            intake.clone(),
            first.service.identity.clone(),
            first.clock.clock(),
        ));
        let phone = common::EngineDevice {
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
    ) -> (HandoverResponse, Option<common::EngineDevice>) {
        // `complete` runs to its store read and waits there: the store is
        // held, so the read cannot finish before the first poll returns.
        // Once the read has returned the row, the revoke runs to completion;
        // then `complete` goes on.
        let hold = StoreHold::new(&self.first.store);
        let woken = Woken::new();
        let mut completing = std::pin::pin!(self.phone.complete(self.id()));
        assert!(
            woken.poll(completing.as_mut()).is_pending(),
            "complete waits on the store read"
        );
        hold.release();
        woken.wait("the store read returns").await;
        self.service.revoke(self.phone.device.id).await.unwrap();
        let again = if pairs_again {
            Some(self.phone.pair_again().await)
        } else {
            None
        };
        (completing.await, again)
    }
}

/// A revoke that lands while `complete` reads the store finds nothing in
/// memory to discard, so the files are still there for the verify; the
/// engine itself must keep a revoked device's recording from the intake.
async fn complete_after_a_revoke_during_its_receipt_read(pairs_again: bool) {
    let restarted = Restarted::new().await;
    let (response, again) = restarted
        .complete_with_a_revoke_during_the_read(pairs_again)
        .await;

    assert_eq!(
        response.status.as_u16(),
        401,
        "the phone learns it is unpaired"
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
    complete_after_a_revoke_during_its_receipt_read(false).await;
}

#[tokio::test]
async fn a_phone_that_paired_again_does_not_let_the_old_complete_through() {
    complete_after_a_revoke_during_its_receipt_read(true).await;
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
    let before = common::EngineDevice {
        service: restarted.first.service.clone(),
        device: restarted.phone.device.clone(),
    };
    let admitted = before.complete(restarted.id()).await;
    assert_eq!(admitted.status.as_u16(), 200);
    let meeting: wire::CompleteResponse = admitted.decode().unwrap();

    let (response, _) = restarted.complete_with_a_revoke_during_the_read(true).await;

    assert_eq!(response.status.as_u16(), 200, "the phone keeps its meeting");
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
    let mut revoking = std::pin::pin!(restarted.service.revoke(restarted.phone.device.id));
    assert!(
        woken.poll(revoking.as_mut()).is_pending(),
        "the revoke waits on its store delete"
    );
    let mut completing = std::pin::pin!(restarted.phone.complete(restarted.id()));
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
    let chunk_size: i64 = 64 * 1024;
    let intake = common::ScriptedIntake::new(Uuid::new_v4(), 0);
    let test = TestService::with(common::Options {
        chunk_size,
        intake: Some(intake.clone() as Arc<dyn steno_core::HandoverIntake>),
        start: false,
        ..common::Options::default()
    })
    .await;
    let phone = common::EngineDevice::paired(&test, "Direct iPhone").await;
    let bytes = seeded_bytes(2 * chunk_size as usize, 96);
    let metadata = phone.metadata(&bytes, chunk_size);
    phone.upload_all(&metadata, &bytes).await;
    let id = metadata.recording_id;
    let inbox = test.inbox();

    // `complete` waits on its `verifying` write and holds the turn of the
    // receipt saves meanwhile.
    let hold = StoreHold::new(&test.store);
    let woken = Woken::new();
    let mut completing = std::pin::pin!(phone.complete(id));
    assert!(
        woken.poll(completing.as_mut()).is_pending(),
        "complete waits on its verifying write"
    );
    let mut revoking = std::pin::pin!(test.service.revoke(phone.device.id));
    assert!(
        woken.poll(revoking.as_mut()).is_pending(),
        "the revoke waits on its store delete"
    );
    assert!(!inbox.has_partial(id), "the revoke discarded the partial");
    hold.release();
    revoking.await.unwrap();
    let again = phone.pair_again().await;

    // The new announce creates the partial, then waits for the turn the old
    // `complete` holds.
    let announced = Woken::new();
    let mut announcing = std::pin::pin!(again.announce(&metadata));
    loop {
        assert!(
            announced.poll(announcing.as_mut()).is_pending(),
            "the announce waits for the old complete's turn"
        );
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

/// Holds the store's one connection from another thread until
/// [`StoreHold::release`], so a store call of the engine waits on it.
struct StoreHold {
    release: mpsc::Sender<()>,
    holder: std::thread::JoinHandle<()>,
}

impl StoreHold {
    fn new(store: &Arc<Store>) -> StoreHold {
        let (held, store_is_held) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let store = store.clone();
        let holder = std::thread::spawn(move || {
            store
                .write(|_| {
                    held.send(()).unwrap();
                    // A test that failed meanwhile drops the sender.
                    let _ = released.recv();
                    Ok(())
                })
                .unwrap();
        });
        store_is_held
            .recv_timeout(common::SIGNAL_BOUND)
            .expect("the store is held");
        StoreHold { release, holder }
    }

    fn release(self) {
        self.release.send(()).unwrap();
        self.holder.join().unwrap();
    }
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
