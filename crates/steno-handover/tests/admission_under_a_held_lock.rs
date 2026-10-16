//! A phone admission while another connection holds the database's write
//! lock, through the real `RecordingIntake` behind the real handover
//! service (`.plans/2026-10-12-store-off-the-workers.md`, the admission
//! under a held lock). `complete` never answers 200 unless the meeting is
//! durable, since the phone then deletes its copy; a refusal leaves no
//! meeting row, no ledger row and no copy that nothing notes, and the
//! phone's retry once the lock goes admits the recording exactly once. An
//! admission whose request is dropped (a phone that hung up mid-admit)
//! still commits, is enqueued once, and the retry is answered from it.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::large_futures
)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{EngineDevice, Options, Phone, TestService, seeded_bytes};
use steno_core::testing::WriteLockHold;
use steno_core::{HandoverIntake as _, HandoverStateKind, Store};
use steno_handover::wire;
use steno_pipeline::{AdmissionFolders, RecordingIntake};
use tokio::sync::Notify;
use uuid::Uuid;

const CHUNK_SIZE: i64 = 128 * 1024;

/// How long a test waits before it fails as hung, as steno-audio's
/// `HANG_GUARD`: every wait here ends once a busy timeout or two ran out.
const HANG_GUARD: Duration = Duration::from_secs(120);

/// The admission folders the intake noted and not yet settled. With
/// [`Noted::lock_at_the_next_copy`], the next `copying` takes the write
/// lock from another connection, so the admission's commit meets it.
#[derive(Default)]
struct Noted {
    folders: Mutex<BTreeMap<Uuid, PathBuf>>,
    lock_at_copy: Mutex<Option<PathBuf>>,
    held: Mutex<Option<WriteLockHold>>,
    copying: Notify,
}

impl AdmissionFolders for Noted {
    fn copying(&self, meeting_id: Uuid, audio_folder: &Path) -> std::io::Result<()> {
        self.folders
            .lock()
            .unwrap()
            .insert(meeting_id, audio_folder.to_path_buf());
        if let Some(database) = self.lock_at_copy.lock().unwrap().take() {
            *self.held.lock().unwrap() = Some(WriteLockHold::new(&database));
        }
        self.copying.notify_one();
        Ok(())
    }

    fn settled(&self, meeting_id: Uuid) {
        self.folders.lock().unwrap().remove(&meeting_id);
    }
}

impl Noted {
    fn len(&self) -> usize {
        self.folders.lock().unwrap().len()
    }

    /// The next `copying` takes the write lock on `database`.
    fn lock_at_the_next_copy(&self, database: &Path) {
        *self.lock_at_copy.lock().unwrap() = Some(database.to_path_buf());
    }

    /// Lets the lock `copying` took go.
    fn release(&self) {
        drop(
            self.held
                .lock()
                .unwrap()
                .take()
                .expect("the lock was taken"),
        );
    }
}

struct World {
    dir: tempfile::TempDir,
    path: PathBuf,
    store: Arc<Store>,
    noted: Arc<Noted>,
    enqueued: Arc<AtomicUsize>,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steno.sqlite");
    let store = Arc::new(Store::open(&path).unwrap());
    let mut settings = store.settings().unwrap();
    settings.audio_folder = steno_core::paths::file_url(&dir.path().join("audio"), true);
    store.save_settings(&settings).unwrap();
    World {
        dir,
        path,
        store,
        noted: Arc::default(),
        enqueued: Arc::default(),
    }
}

impl World {
    fn intake(&self) -> RecordingIntake {
        let enqueued = self.enqueued.clone();
        RecordingIntake::new(
            self.store.clone(),
            Arc::new(move |_, _| {
                enqueued.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }),
            Arc::new(chrono::Utc::now),
            chrono::FixedOffset::east_opt(0).unwrap(),
        )
        .noting_folders_in(self.noted.clone())
    }

    /// The handover service over this world's store and intake.
    async fn service(&self, start: bool) -> TestService {
        TestService::with(Options {
            chunk_size: CHUNK_SIZE,
            intake: Some(Arc::new(self.intake())),
            store: Some(self.store.clone()),
            start,
            ..Options::default()
        })
        .await
    }

    fn count(&self, table: &str) -> i64 {
        self.store
            .read(|connection| {
                Ok(
                    connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .unwrap()
    }

    /// Every file under the audio folder.
    fn audio_files(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, into: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, into);
                } else {
                    into.push(path);
                }
            }
        }
        let mut files = Vec::new();
        walk(&self.dir.path().join("audio"), &mut files);
        files
    }

    fn receipt(&self, recording_id: Uuid) -> Option<HandoverStateKind> {
        self.store
            .handover_receipt(recording_id)
            .unwrap()
            .map(|receipt| receipt.state.kind())
    }

    /// The lock held from another connection, let go after `hold` on a
    /// thread of its own.
    fn hold_for(&self, hold: Duration) -> std::thread::JoinHandle<()> {
        let lock = WriteLockHold::new(&self.path);
        std::thread::spawn(move || {
            std::thread::sleep(hold);
            drop(lock);
        })
    }

    /// One meeting, one ledger row and one enqueue.
    fn admitted_once(&self) {
        assert_eq!(self.count("meeting"), 1, "one meeting");
        assert_eq!(self.count("handoverAdmission"), 1, "one ledger row");
        assert_eq!(self.enqueued.load(Ordering::SeqCst), 1, "enqueued once");
    }

    /// Drops `call`, an admission, once its copy took the write lock from
    /// another connection, so its commit waits; then lets the lock go.
    async fn dropped_at_the_copy<T: std::fmt::Debug>(&self, call: impl Future<Output = T>) {
        self.noted.lock_at_the_next_copy(&self.path);
        {
            let mut call = pin!(call);
            tokio::select! {
                answered = &mut call => panic!("answered under the lock: {answered:?}"),
                () = bounded("the copy", self.noted.copying.notified()) => {}
            }
        }
        self.noted.release();
    }

    /// Waits until the intake enqueued `count` meetings.
    async fn enqueued(&self, count: usize) {
        let deadline = Instant::now() + HANG_GUARD;
        while self.enqueued.load(Ordering::SeqCst) < count {
            assert!(Instant::now() < deadline, "enqueued within {HANG_GUARD:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// `what` within [`HANG_GUARD`].
async fn bounded<T>(what: &str, call: impl Future<Output = T>) -> T {
    tokio::time::timeout(HANG_GUARD, call)
        .await
        .unwrap_or_else(|_| panic!("{what} within {HANG_GUARD:?}"))
}

/// A paired phone uploads, then `complete` runs while the lock is held
/// for `hold`; returns the world, the service, the phone, the recording's
/// metadata and the first answer's status.
async fn complete_under(
    hold: Duration,
) -> (
    World,
    TestService,
    Phone,
    steno_core::RecordingMetadata,
    u16,
) {
    let world = world();
    let test = world.service(true).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 41);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    phone.upload_all(&metadata, &bytes).await;

    let released = world.hold_for(hold);
    let first = bounded("the first complete", phone.complete(metadata.recording_id)).await;
    released.join().unwrap();
    if first.status == 200 {
        // The phone deletes its copy on a 200: the meeting must be durable.
        let meeting_id = first.json::<wire::CompleteResponse>().meeting_id;
        assert!(
            world.store.meeting(meeting_id).unwrap().is_some(),
            "200 without a meeting row"
        );
    }
    (world, test, phone, metadata, first.status)
}

/// The phone's retry and a replay once the lock went: one meeting, one
/// ledger row, one enqueue, an announce answered delivered, and no copy
/// on the disk that no meeting owns and nothing notes.
async fn the_retry_admits_once(
    world: &World,
    test: &TestService,
    phone: &Phone,
    metadata: &steno_core::RecordingMetadata,
) {
    let id = metadata.recording_id;
    let second = bounded("the retry", phone.complete(id)).await;
    assert_eq!(second.status, 200, "the retry admits");
    let meeting_id = second.json::<wire::CompleteResponse>().meeting_id;
    let third = bounded("the replay", phone.complete(id)).await;
    assert_eq!(third.status, 200);
    assert_eq!(
        third.json::<wire::CompleteResponse>().meeting_id,
        meeting_id
    );
    assert!(world.store.meeting(meeting_id).unwrap().is_some());
    world.admitted_once();
    let delivered = bounded("the announce", phone.announce(metadata)).await;
    assert_eq!(
        delivered.json::<wire::RecordingStatus>().state,
        HandoverStateKind::Complete
    );
    test.stop().await;
    let owned = world.store.asset(meeting_id).unwrap().unwrap().url;
    let stray = world
        .audio_files()
        .into_iter()
        .filter(|file| !owned.contains(file.file_name().unwrap().to_str().unwrap()))
        .count();
    assert_eq!(
        stray,
        world.noted.len(),
        "a stray copy is noted for the launch"
    );
}

/// A seven-second hold: the `verifying` write runs out one busy timeout,
/// then the admission commits once the lock goes, so the phone gets a 200
/// with a durable meeting, one ledger row and one copy, and a replay
/// answers the same meeting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_complete_under_a_seven_second_hold_answers_with_a_durable_meeting() {
    let (world, test, phone, metadata, status) = complete_under(Duration::from_secs(7)).await;
    let id = metadata.recording_id;
    assert_eq!(status, 200, "admitted once the lock went");
    assert_eq!(
        (
            world.count("meeting"),
            world.count("handoverAdmission"),
            world.audio_files().len()
        ),
        (1, 1, 1),
        "one admission"
    );
    assert_eq!(world.receipt(id), Some(HandoverStateKind::Complete));
    the_retry_admits_once(&world, &test, &phone, &metadata).await;
}

/// A twelve-second hold: the `verifying` write and the admission's commit
/// each run out a busy timeout, so the phone gets an error with no
/// meeting, no ledger row and no copy; the `failed` save lands once the
/// lock goes. The retry admits the recording exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_complete_under_a_twelve_second_hold_fails_cleanly_and_its_retry_admits_once() {
    let (world, test, phone, metadata, status) = complete_under(Duration::from_secs(12)).await;
    let id = metadata.recording_id;
    assert_ne!(status, 200, "refused while the lock was held");
    assert_eq!(
        (
            world.count("meeting"),
            world.count("handoverAdmission"),
            world.audio_files().len(),
            world.noted.len()
        ),
        (0, 0, 0, 0),
        "a refused admission leaves no row and no copy"
    );
    assert_eq!(world.receipt(id), Some(HandoverStateKind::Failed));
    the_retry_admits_once(&world, &test, &phone, &metadata).await;
}

/// A device paired in `world`'s store and a recording file for the intake
/// alone.
fn upload(
    world: &World,
) -> (
    steno_core::PairedDevice,
    steno_core::RecordingMetadata,
    PathBuf,
) {
    let device = steno_core::PairedDevice {
        id: Uuid::new_v4(),
        name: "Phone".to_owned(),
        paired_at: chrono::Utc::now(),
        last_seen_at: None,
    };
    world.store.save_paired_device(&device, &[1; 32]).unwrap();
    let bytes = seeded_bytes(9000, 5);
    let metadata = steno_core::RecordingMetadata {
        recording_id: Uuid::new_v4(),
        started_at: chrono::Utc::now(),
        duration_seconds: 12.0,
        byte_count: bytes.len() as i64,
        sha256: steno_core::content_hash::sha256(&bytes),
        chunk_size: 9000,
        format: steno_core::AudioFormat::M4aAac,
        device_name: "Phone".to_owned(),
    };
    let file = world.dir.path().join("upload.m4a");
    std::fs::write(&file, &bytes).unwrap();
    (device, metadata, file)
}

/// The intake alone, the lock held for `hold` from the moment `admit` is
/// asked, so the admission's commit meets it: an error with no row, the
/// upload kept, and no copy once the `failed` save landed; then a retry
/// and a replay admit once.
async fn intake_alone(hold: Duration) {
    let world = world();
    let (device, metadata, file) = upload(&world);
    let intake = world.intake();

    let released = world.hold_for(hold);
    let first = bounded("the admit", intake.admit(&file, &metadata, &device)).await;
    released.join().unwrap();
    assert!(first.is_err(), "the commit ran out its busy timeout");
    assert!(file.exists(), "the upload stays for the retry");
    assert_eq!(
        (
            world.count("meeting"),
            world.count("handoverAdmission"),
            world.audio_files().len(),
            world.noted.len()
        ),
        (0, 0, 0, 0),
        "no row, and the copy went once the failed save landed"
    );
    assert_eq!(
        world.receipt(metadata.recording_id),
        Some(HandoverStateKind::Failed)
    );
    let retried = intake.admit(&file, &metadata, &device).await.unwrap();
    let again = intake.admit(&file, &metadata, &device).await.unwrap();
    assert_eq!(retried, again, "a replay answers the same meeting");
    world.admitted_once();
    assert_eq!(world.audio_files().len(), 1, "one copy");
}

/// Seven seconds: the commit runs out its busy timeout, and the `failed`
/// save lands once the lock goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_intake_under_a_seven_second_hold_fails_cleanly_and_admits_once() {
    intake_alone(Duration::from_secs(7)).await;
}

/// Twelve seconds: the commit and the first `failed` save run out their
/// busy timeouts; the save is tried again
/// ([`RecordingIntake::FAILED_SAVE_ATTEMPTS`]) and lands once the lock
/// goes, so the copy goes too.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_intake_under_a_twelve_second_hold_tries_the_failed_save_again() {
    intake_alone(Duration::from_secs(12)).await;
}

/// An `admit` dropped while its commit waits on the lock: the admission
/// still commits once the lock goes, and the meeting is enqueued once.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_dropped_admit_still_commits_and_enqueues() {
    let world = world();
    let (device, metadata, file) = upload(&world);
    let intake = world.intake();
    world
        .dropped_at_the_copy(intake.admit(&file, &metadata, &device))
        .await;
    world.enqueued(1).await;
    let meeting_id = bounded("the replay", intake.admit(&file, &metadata, &device))
        .await
        .unwrap();
    assert!(world.store.meeting(meeting_id).unwrap().is_some());
    world.admitted_once();
}

/// A `complete` dropped mid-admit, as a phone that hangs up does: the
/// admission commits once the lock goes, is enqueued once, and the
/// receipt ends `complete`, so the phone's retry gets a 200 with that
/// meeting, not a 409 that asks for the chunks again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_complete_dropped_mid_admit_leaves_its_meeting_and_the_retry_is_answered_from_it() {
    let world = world();
    let test = world.service(false).await;
    let phone = EngineDevice::paired(&test, "Phone").await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 43);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    phone.upload_all(&metadata, &bytes).await;
    let id = metadata.recording_id;
    let mut receipts = test.service.receipts();

    world.dropped_at_the_copy(phone.complete(id)).await;
    world.enqueued(1).await;
    bounded(
        "the receipt completes",
        receipts.wait_for(|receipts| {
            receipts.iter().any(|receipt| {
                receipt.recording_id == id && receipt.state.kind() == HandoverStateKind::Complete
            })
        }),
    )
    .await
    .unwrap();

    let retry = bounded("the retry", phone.complete(id)).await;
    assert_eq!(retry.status.as_u16(), 200, "answered from the admission");
    let answered: wire::CompleteResponse = serde_json::from_slice(&retry.body).unwrap();
    assert!(world.store.meeting(answered.meeting_id).unwrap().is_some());
    world.admitted_once();
    assert_eq!(world.audio_files().len(), 1, "one copy");
}
