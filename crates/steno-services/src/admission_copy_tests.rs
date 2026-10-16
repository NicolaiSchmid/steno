//! A phone admission that gave up under a write lock another process held
//! (its commit and its `failed` save both ran out the busy timeout), then
//! a relaunch and the phone's retry, in either order: the recording is one
//! meeting with one ledger row, and its audio is kept
//! (`.plans/2026-10-07-stable-promotion.md`, "The copy an admission left
//! is one meeting"). The copy is noted with the upload it holds
//! ([`crate::audio_folders::uploads`]): the launch adopts it with the
//! upload's ledger row, and a retry before the launch admits into it. A
//! note an older build wrote names no upload and keeps the adoption it
//! had. The writer's busy timeout is 20 ms in place of five seconds
//! (`Store::on_writer`), so an admission gives up within a second rather
//! than after a hold of more than 20 s. Rust only.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use steno_core::testing::WriteLockHold;
use steno_core::{
    HandoverIntake as _, HandoverReceipt, HandoverState, PairedDevice, RecordingMetadata, Store,
};
use uuid::Uuid;

use crate::App;
use crate::testing::{HANG_GUARD, an_hour_later, app_over_fakes, database_in, synthetic_capture};

/// A paired phone, its verified upload in the inbox and its `verifying`
/// receipt, over a store in `dir`.
struct World {
    dir: tempfile::TempDir,
    device: PairedDevice,
    metadata: RecordingMetadata,
    upload: PathBuf,
    bytes: Vec<u8>,
}

impl World {
    fn new() -> (World, App) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(database_in(dir.path())).unwrap());
        let app = app_over_fakes(dir.path(), &store, synthetic_capture());
        let now = chrono::Utc::now();
        let device = PairedDevice {
            id: Uuid::new_v4(),
            name: "Phone".to_owned(),
            paired_at: now,
            last_seen_at: None,
        };
        store.save_paired_device(&device, &[1; 32]).unwrap();
        let bytes = steno_pipeline::fixtures::wav_data(&vec![0; 16_000], 16_000, 1);
        let metadata = RecordingMetadata {
            recording_id: Uuid::new_v4(),
            started_at: now,
            duration_seconds: 1.0,
            byte_count: i64::try_from(bytes.len()).unwrap(),
            sha256: steno_core::content_hash::sha256(&bytes),
            chunk_size: 128 * 1024,
            format: steno_core::AudioFormat::Wav16kInt16,
            device_name: "Phone".to_owned(),
        };
        store
            .save_handover_receipt(&HandoverReceipt {
                recording_id: metadata.recording_id,
                device_id: device.id,
                state: HandoverState::Verifying,
                byte_count: metadata.byte_count,
                sha256: metadata.sha256.clone(),
                chunk_size: metadata.chunk_size,
                received_chunks: vec![0],
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let upload = inbox.join("upload.wav");
        std::fs::write(&upload, &bytes).unwrap();
        let world = World {
            dir,
            device,
            metadata,
            upload,
            bytes,
        };
        (world, app)
    }

    fn support(&self) -> PathBuf {
        self.dir.path().join("support")
    }

    fn database(&self) -> PathBuf {
        database_in(self.dir.path())
    }

    /// The phone's `complete` through the app's intake, bounded by
    /// [`HANG_GUARD`].
    async fn admit(&self, app: &App) -> steno_core::protocols::BoundaryResult<Uuid> {
        let intake = crate::app::handover_intake(
            app.store.clone(),
            app.pipeline.clone(),
            app.zone,
            &self.support(),
        );
        tokio::time::timeout(
            HANG_GUARD,
            intake.admit(&self.upload, &self.metadata, &self.device),
        )
        .await
        .expect("the admission ends within the guard")
    }

    /// An admission that gives up under a write lock another connection
    /// takes and still holds, returned: the busy commit and the `failed`
    /// save that follows. Also returns the meeting id its copy is noted
    /// under, read as an older build reads the record.
    async fn gave_up(&self, app: &App) -> (Uuid, WriteLockHold) {
        let hold = WriteLockHold::new(&self.database());
        app.store
            .on_writer(|connection| connection.busy_timeout(Duration::from_millis(20)))
            .unwrap();
        let error = self.admit(app).await.unwrap_err();
        assert!(error.to_string().contains("locked"), "{error}");
        let record = std::fs::read(self.support().join(crate::audio_folders::RECORDED_FILE))
            .expect("the copy's folder is noted");
        let noted: BTreeMap<Uuid, PathBuf> = serde_json::from_slice(&record).unwrap();
        let [(meeting_id, _)] = noted.into_iter().collect::<Vec<_>>()[..] else {
            panic!("one noted copy");
        };
        assert_eq!(self.copies(), [meeting_id], "one copy");
        assert!(self.upload.exists(), "the upload stays for the retry");
        (meeting_id, hold)
    }

    /// The meeting folders in the audio folder that hold the upload.
    fn copies(&self) -> Vec<Uuid> {
        let audio = self.dir.path().join("audio");
        let Ok(entries) = std::fs::read_dir(&audio) else {
            return Vec::new();
        };
        let mut copies: Vec<Uuid> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| Uuid::parse_str(entry.file_name().to_str()?).ok())
            .collect();
        copies.sort();
        copies
    }

    /// Exactly one meeting, `meeting_id`, with one ledger row for the
    /// upload, and the upload's bytes in its asset's master.
    fn assert_one_meeting(&self, store: &Store, meeting_id: Uuid) {
        let meetings: Vec<Uuid> = store
            .all_meetings()
            .unwrap()
            .iter()
            .map(|meeting| meeting.id)
            .collect();
        assert_eq!(meetings, [meeting_id], "one meeting");
        let ledger: i64 = store
            .read(|connection| {
                Ok(
                    connection.query_row("SELECT COUNT(*) FROM handoverAdmission", [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(ledger, 1, "one ledger row");
        assert_eq!(
            store
                .admitted_meeting(
                    self.metadata.recording_id,
                    self.metadata.byte_count,
                    &self.metadata.sha256,
                )
                .unwrap(),
            Some(meeting_id),
            "the ledger answers the phone with it"
        );
        let asset = store.asset(meeting_id).unwrap().expect("its asset");
        let master = steno_core::paths::file_url_path(&asset.url).unwrap();
        assert_eq!(
            std::fs::read(master).unwrap(),
            self.bytes,
            "the audio is kept"
        );
        assert_eq!(self.copies(), [meeting_id], "no second copy");
    }
}

/// The app over the same directory again, launched as the product
/// launches, once its launch work ran.
async fn relaunch(world: &World) -> App {
    let store = Arc::new(Store::open(world.database()).unwrap());
    let mut app = app_over_fakes(world.dir.path(), &store, synthetic_capture());
    app.live_recording_check = an_hour_later();
    let host = Arc::new(app.host().unwrap());
    app.launch(&host);
    tokio::time::timeout(HANG_GUARD, app.launch_finished())
        .await
        .expect("the launch work within the guard");
    app
}

/// `app` quit, its runs done first so none writes after.
async fn quit(app: App) {
    tokio::time::timeout(HANG_GUARD, app.pipeline.current().wait_until_idle())
        .await
        .expect("the runs end within the guard");
    app.shutdown();
}

/// The launch first: it adopts the noted copy with the upload's ledger
/// row, and the phone's retry is answered with that meeting, its new copy
/// removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_left_by_an_admission_that_gave_up_is_adopted_and_the_retry_gets_its_meeting() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    quit(app).await;

    let app = relaunch(&world).await;
    assert_eq!(
        app.store.meeting(noted).unwrap().map(|m| m.id),
        Some(noted),
        "adopted"
    );
    world.assert_one_meeting(&app.store, noted);

    assert_eq!(world.admit(&app).await.unwrap(), noted, "the retry");
    world.assert_one_meeting(&app.store, noted);
    quit(app).await;
}

/// The retry first, while the lock is still held and once it went: it
/// admits into the noted copy and makes no second one, and the next launch
/// has nothing left to adopt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retry_admits_into_the_copy_an_admission_that_gave_up_left() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;

    world.admit(&app).await.unwrap_err();
    assert_eq!(world.copies(), [noted], "no second copy under the lock");
    assert!(world.upload.exists());
    drop(hold);

    assert_eq!(
        world.admit(&app).await.unwrap(),
        noted,
        "into the noted copy"
    );
    world.assert_one_meeting(&app.store, noted);
    quit(app).await;

    let app = relaunch(&world).await;
    world.assert_one_meeting(&app.store, noted);
    assert!(
        crate::audio_folders::recorded(&world.support())
            .unwrap()
            .is_empty(),
        "nothing left noted"
    );
    quit(app).await;
}

/// A note an older build wrote names no upload: the launch adopts the
/// copy as before, with no ledger row, and the phone's retry admits the
/// recording again, as a second meeting. Nothing is lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_noted_without_its_upload_is_adopted_as_before() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    quit(app).await;
    std::fs::remove_file(world.support().join(crate::audio_folders::UPLOADS_FILE)).unwrap();

    let app = relaunch(&world).await;
    assert!(app.store.meeting(noted).unwrap().is_some(), "adopted");
    assert_eq!(
        app.store
            .admitted_meeting(
                world.metadata.recording_id,
                world.metadata.byte_count,
                &world.metadata.sha256
            )
            .unwrap(),
        None,
        "with no ledger row"
    );

    let retried = world.admit(&app).await.unwrap();
    assert_ne!(retried, noted, "admitted again");
    assert_eq!(app.store.all_meetings().unwrap().len(), 2);
    assert_eq!(world.copies().len(), 2, "both copies kept");
    quit(app).await;
}
