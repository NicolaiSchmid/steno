//! A phone admission that gave up under a write lock another process held
//! (its commit and its `failed` save both ran out the busy timeout), then
//! a relaunch and the phone's retry, in either order: the recording is one
//! meeting with one ledger row, and its audio is kept
//! (`.plans/2026-10-07-stable-promotion.md`, "P40 follow-up: the copy an
//! admission left is one meeting"). The copy is noted with the upload it holds
//! ([`crate::audio_folders::uploads`]): the launch adopts it with the
//! upload's ledger row, and a retry before the launch admits into it. A
//! copy an older build noted names no upload and is adopted as a
//! recording with no meeting, without a ledger row. The writer's busy timeout is 20 ms in place of five seconds
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
pub(super) struct World {
    pub(super) dir: tempfile::TempDir,
    pub(super) device: PairedDevice,
    pub(super) metadata: RecordingMetadata,
    pub(super) upload: PathBuf,
    pub(super) bytes: Vec<u8>,
}

impl World {
    pub(super) fn new() -> (World, App) {
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

    pub(super) fn support(&self) -> PathBuf {
        self.dir.path().join("support")
    }

    pub(super) fn database(&self) -> PathBuf {
        database_in(self.dir.path())
    }

    /// The phone's `complete` through the app's intake, bounded by
    /// [`HANG_GUARD`].
    pub(super) async fn admit(&self, app: &App) -> steno_core::protocols::BoundaryResult<Uuid> {
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
    pub(super) async fn gave_up(&self, app: &App) -> (Uuid, WriteLockHold) {
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
    pub(super) fn copies(&self) -> Vec<Uuid> {
        meeting_folders_in(&self.dir.path().join("audio"))
    }

    /// Exactly one meeting, `meeting_id`, with one ledger row for the
    /// upload, and the upload's bytes in its asset's master.
    pub(super) fn assert_one_meeting(&self, store: &Store, meeting_id: Uuid) {
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

/// The meeting folders (named by a UUID) in `folder`, sorted.
fn meeting_folders_in(folder: &std::path::Path) -> Vec<Uuid> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut ids: Vec<Uuid> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| Uuid::parse_str(entry.file_name().to_str()?).ok())
        .collect();
    ids.sort();
    ids
}

/// The app over the same directory again, launched as the product
/// launches, once its launch work ran.
pub(super) async fn relaunch(world: &World) -> App {
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
pub(super) async fn quit(app: App) {
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
/// copy as a recording with no meeting, without a ledger row, and the
/// phone's retry admits the recording again, as a second meeting. Nothing
/// is lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_noted_without_its_upload_is_adopted_with_no_ledger_row() {
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

impl World {
    /// The upload's ledger row written as another meeting, with its own
    /// asset elsewhere, as an older build's retry or another device's
    /// upload of the same bytes leaves it; the user then deleted that
    /// meeting when `deleted`. Returns its id.
    fn ledger_holds_the_upload_as_another_meeting(&self, store: &Store, deleted: bool) -> Uuid {
        let meeting = steno_core::Meeting {
            id: Uuid::new_v4(),
            source: steno_core::MeetingSource::Phone,
            state: steno_core::MeetingState::Ready,
            ..steno_core::testing::sample_data::meeting()
        };
        let master = self.dir.path().join("elsewhere").join("master.wav");
        let asset = steno_core::AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: meeting.id,
            url: steno_core::paths::file_url(&master, false),
            format: steno_core::AudioFormat::Wav16kInt16,
            lanes: vec![steno_core::AudioLane::Mixed],
            sidecars_16k: BTreeMap::new(),
            mixdown_url: None,
            retention: steno_core::AudioRetention::KeepForever,
            expires_at: None,
        };
        assert_eq!(
            store
                .adopt_upload_durably(
                    &meeting,
                    &asset,
                    self.metadata.recording_id,
                    self.metadata.byte_count,
                    &self.metadata.sha256,
                )
                .unwrap(),
            steno_core::AdmissionCommit::Wrote
        );
        if deleted {
            store.delete_meeting(meeting.id).unwrap();
        }
        meeting.id
    }

    /// The master of `meeting_id`'s noted copy.
    fn master_of(&self, meeting_id: Uuid) -> PathBuf {
        steno_core::RecordingLayout::new(&self.dir.path().join("audio"), meeting_id)
            .master(self.metadata.format)
    }

    /// The noted copy of `meeting_id` with one byte of its audio changed:
    /// the same size, other bytes.
    fn corrupt(&self, meeting_id: Uuid) -> Vec<u8> {
        let mut bytes = self.bytes.clone();
        *bytes.last_mut().unwrap() ^= 0xFF;
        std::fs::write(self.master_of(meeting_id), &bytes).unwrap();
        bytes
    }

    /// How many folders and uploads are noted.
    pub(super) fn noted(&self) -> (usize, usize) {
        (
            crate::audio_folders::recorded(&self.support())
                .unwrap()
                .len(),
            crate::audio_folders::uploads(&self.support())
                .unwrap()
                .len(),
        )
    }

    /// The meeting ids in `store`.
    pub(super) fn meetings(store: &Store) -> Vec<Uuid> {
        store
            .all_meetings()
            .unwrap()
            .iter()
            .map(|meeting| meeting.id)
            .collect()
    }
}

/// The read of the noted copy's length ended with the app at two launches
/// in a row (`steno_pipeline::ReadingMarks`): the next launch adopts it
/// with the upload's ledger row, failed with
/// [`steno_pipeline::READ_STOPPED`] and unread, and does not process it;
/// the phone's retry is answered with that meeting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_noted_copy_whose_length_read_ended_with_the_app_is_adopted_failed_with_its_ledger_row() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    quit(app).await;
    let store = Store::open(world.database()).unwrap();
    for _ in 0..2 {
        let killed = Arc::new(steno_pipeline::ReadingMarks::at_launch(
            &world.support(),
            &store,
            chrono::Utc::now(),
        ));
        std::mem::forget(killed.reading(
            noted,
            None,
            &world.master_of(noted),
            steno_pipeline::ReadStage::Duration,
        ));
    }
    drop(store);

    let app = relaunch(&world).await;
    world.assert_one_meeting(&app.store, noted);
    let failed = steno_core::MeetingState::Failed {
        reason: steno_pipeline::READ_STOPPED.to_owned(),
    };
    tokio::time::timeout(HANG_GUARD, app.pipeline.current().wait_until_idle())
        .await
        .expect("the runs end within the guard");
    let meeting = app.store.meeting(noted).unwrap().unwrap();
    assert_eq!(meeting.state, failed, "not processed");
    assert_eq!(meeting.duration, 0.0, "its length was not read");

    assert_eq!(world.admit(&app).await.unwrap(), noted, "the retry");
    world.assert_one_meeting(&app.store, noted);
    quit(app).await;
}

/// The ledger holds the noted copy's bytes as a meeting the user deleted:
/// the launch removes the copy's folder, since the user deleted that
/// audio, and forgets its notes. The ledger row stays, so the phone is
/// answered "delivered" from it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_noted_copy_of_bytes_whose_meeting_the_user_deleted_is_removed() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    let deleted = world.ledger_holds_the_upload_as_another_meeting(&app.store, true);
    quit(app).await;

    let app = relaunch(&world).await;
    assert_eq!(
        world.copies(),
        Vec::<Uuid>::new(),
        "the copy's folder is removed"
    );
    assert!(!world.master_of(noted).exists());
    assert_eq!(World::meetings(&app.store), Vec::<Uuid>::new());
    assert_eq!(world.noted(), (0, 0));
    assert_eq!(
        app.store
            .admitted_meeting(
                world.metadata.recording_id,
                world.metadata.byte_count,
                &world.metadata.sha256
            )
            .unwrap(),
        Some(deleted),
        "the ledger row stays"
    );
    quit(app).await;
}

/// The ledger holds the noted copy's bytes as a meeting that still
/// exists: the copy is neither adopted nor removed, and keeps its entries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_noted_copy_of_bytes_whose_meeting_exists_is_kept() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    let existing = world.ledger_holds_the_upload_as_another_meeting(&app.store, false);
    quit(app).await;

    let app = relaunch(&world).await;
    assert_eq!(world.copies(), [noted], "the copy is kept");
    assert_eq!(std::fs::read(world.master_of(noted)).unwrap(), world.bytes);
    assert_eq!(World::meetings(&app.store), [existing]);
    assert_eq!(world.noted(), (1, 1), "with its entries");
    quit(app).await;
}

/// The meeting that holds the noted copy's bytes exists in a state other
/// than ready (its processing failed, so its own master may not be
/// readable): the copy is kept with its entries, neither adopted nor
/// removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_copy_of_bytes_whose_meeting_failed_is_kept() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    let existing = world.ledger_holds_the_upload_as_another_meeting(&app.store, false);
    app.store
        .set_state(
            existing,
            steno_core::MeetingState::Failed {
                reason: "transcription".to_owned(),
            },
            chrono::Utc::now(),
        )
        .unwrap();
    quit(app).await;

    let app = relaunch(&world).await;
    assert_eq!(world.copies(), [noted], "the copy is kept");
    assert_eq!(std::fs::read(world.master_of(noted)).unwrap(), world.bytes);
    assert_eq!(World::meetings(&app.store), [existing]);
    assert_eq!(world.noted(), (1, 1), "with its entries");
    quit(app).await;
}

/// While the meeting that holds the noted copy's bytes exists, the copy
/// keeps its entries, and neither the retry nor a launch makes it a second
/// meeting or a second copy. Once the user deletes that meeting through
/// the host, the next launch removes the copy and forgets its entries; the
/// ledger row stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kept_copy_goes_once_the_user_deletes_the_meeting_holding_its_bytes() {
    use steno_bridge::BridgeHost as _;
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    let other = world.ledger_holds_the_upload_as_another_meeting(&app.store, false);
    quit(app).await;

    let app = relaunch(&world).await;
    assert_eq!(world.copies(), [noted], "the copy is kept");
    assert_eq!(World::meetings(&app.store), [other], "not adopted");
    assert_eq!(world.noted(), (1, 1), "with its entries");
    assert_eq!(
        world.admit(&app).await.unwrap(),
        other,
        "the retry gets that meeting"
    );
    assert_eq!(World::meetings(&app.store), [other], "no second meeting");
    assert_eq!(world.copies(), [noted], "no second copy");
    assert_eq!(world.noted(), (1, 1));
    quit(app).await;

    // A launch whose host confirms the delete, as the user does.
    let store = Arc::new(Store::open(world.database()).unwrap());
    let mut app = app_over_fakes(world.dir.path(), &store, synthetic_capture());
    app.live_recording_check = an_hour_later();
    let host = Arc::new(
        app.host()
            .unwrap()
            .with_dialogs(Box::new(|_| true), Box::new(|_| None)),
    );
    app.launch(&host);
    tokio::time::timeout(HANG_GUARD, app.launch_finished())
        .await
        .expect("the launch work within the guard");
    assert_eq!(World::meetings(&app.store), [other], "no second meeting");
    assert_eq!(world.copies(), [noted]);
    assert_eq!(world.noted(), (1, 1));
    let reply = host
        .meetings_delete(steno_bridge::MeetingIdParams { meeting_id: other })
        .unwrap();
    assert!(reply.confirmed);
    assert_eq!(World::meetings(&app.store), Vec::<Uuid>::new(), "deleted");
    assert_eq!(
        world.copies(),
        [noted],
        "the delete leaves the copy to the launch"
    );
    drop(host);
    quit(app).await;

    let app = relaunch(&world).await;
    assert_eq!(world.copies(), Vec::<Uuid>::new(), "the copy goes");
    assert_eq!(World::meetings(&app.store), Vec::<Uuid>::new());
    assert_eq!(world.noted(), (0, 0));
    assert_eq!(
        app.store
            .admitted_meeting(
                world.metadata.recording_id,
                world.metadata.byte_count,
                &world.metadata.sha256
            )
            .unwrap(),
        Some(other),
        "the ledger row stays"
    );
    quit(app).await;
}

/// The removed copy's folder also holds a file Steno did not write: the
/// launch removes the copy's master and keeps that file and its folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unrelated_file_in_a_removed_copys_folder_is_kept() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    let unrelated = world.master_of(noted).with_file_name("notes.txt");
    std::fs::write(&unrelated, b"the user's own file").unwrap();
    world.ledger_holds_the_upload_as_another_meeting(&app.store, true);
    quit(app).await;

    let app = relaunch(&world).await;
    assert!(!world.master_of(noted).exists(), "the deleted audio goes");
    assert_eq!(
        std::fs::read(&unrelated).unwrap(),
        b"the user's own file",
        "a file Steno did not write stays"
    );
    assert_eq!(World::meetings(&app.store), Vec::<Uuid>::new());
    assert_eq!(
        world.noted(),
        (0, 0),
        "its master is gone, so are its entries"
    );
    quit(app).await;

    let app = relaunch(&world).await;
    assert_eq!(
        World::meetings(&app.store),
        Vec::<Uuid>::new(),
        "not adopted later"
    );
    assert!(unrelated.exists());
    quit(app).await;
}

/// The copy sits in an audio folder the user chose, beside a file of the
/// user's and a meeting folder another install wrote, and the ledger
/// holds its bytes as a meeting the user deleted: only the copy's own
/// folder goes, also when the setting changed after the copy
/// (`change_setting_after`).
async fn removes_a_copy_in_a_chosen_audio_folder_alone(change_setting_after: bool) {
    let (world, app) = World::new();
    let chosen = world.dir.path().join("Chosen Audio");
    std::fs::create_dir_all(&chosen).unwrap();
    let mut settings = app.store.settings().unwrap();
    settings.audio_folder = steno_core::paths::file_url(&chosen, true);
    app.store.save_settings(&settings).unwrap();
    let hold = WriteLockHold::new(&world.database());
    app.store
        .on_writer(|connection| connection.busy_timeout(Duration::from_millis(20)))
        .unwrap();
    world.admit(&app).await.unwrap_err();
    drop(hold);
    let [noted] = meeting_folders_in(&chosen)[..] else {
        panic!("one copy in the chosen folder");
    };
    assert_eq!(
        world.copies(),
        Vec::<Uuid>::new(),
        "none in the default folder"
    );
    let copy = steno_core::RecordingLayout::new(&chosen, noted);
    assert!(copy.master(world.metadata.format).exists());
    let users = chosen.join("keep.txt");
    std::fs::write(&users, b"user").unwrap();
    let foreign_id = Uuid::new_v4();
    let foreign = steno_core::RecordingLayout::new(&chosen, foreign_id);
    std::fs::create_dir_all(&foreign.directory).unwrap();
    std::fs::write(foreign.master(world.metadata.format), &world.bytes).unwrap();
    if change_setting_after {
        let elsewhere = world.dir.path().join("Later Audio");
        std::fs::create_dir_all(&elsewhere).unwrap();
        settings.audio_folder = steno_core::paths::file_url(&elsewhere, true);
        app.store.save_settings(&settings).unwrap();
    }
    world.ledger_holds_the_upload_as_another_meeting(&app.store, true);
    quit(app).await;

    let app = relaunch(&world).await;
    assert!(!copy.directory.exists(), "the copy's folder goes");
    assert!(users.exists(), "the chosen folder and the user's file stay");
    assert_eq!(
        meeting_folders_in(&chosen),
        [foreign_id],
        "another install's meeting folder stays"
    );
    assert!(foreign.master(world.metadata.format).exists());
    assert_eq!(World::meetings(&app.store), Vec::<Uuid>::new());
    assert_eq!(world.noted(), (0, 0));
    quit(app).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_in_a_chosen_audio_folder_goes_alone() {
    removes_a_copy_in_a_chosen_audio_folder_alone(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_in_a_chosen_audio_folder_goes_alone_after_the_setting_changed() {
    removes_a_copy_in_a_chosen_audio_folder_alone(true).await;
}

/// The noted copy holds other bytes of the same size, and the ledger
/// holds the upload as a meeting the user deleted: the copy is not the
/// upload, so it is kept and adopted as a recording with no meeting,
/// without a ledger row, and stays noted until its row is durable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_noted_copy_holding_other_bytes_is_kept_though_the_uploads_meeting_was_deleted() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    world.ledger_holds_the_upload_as_another_meeting(&app.store, true);
    let other_bytes = world.corrupt(noted);
    quit(app).await;

    let app = relaunch(&world).await;
    assert_eq!(world.copies(), [noted], "the copy is kept");
    assert_eq!(std::fs::read(world.master_of(noted)).unwrap(), other_bytes);
    assert_eq!(World::meetings(&app.store), [noted], "adopted");
    assert_eq!(world.noted(), (1, 1), "still noted");
    quit(app).await;
}

/// The noted copy cannot be read, and the ledger holds the upload as a
/// meeting the user deleted: the copy is kept with its notes for the next
/// launch, neither adopted nor removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_noted_copy_that_cannot_be_read_is_kept_though_the_uploads_meeting_was_deleted() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    world.ledger_holds_the_upload_as_another_meeting(&app.store, true);
    quit(app).await;
    crate::recovery::UNREADABLE_COPIES
        .lock()
        .unwrap()
        .push(noted);

    let app = relaunch(&world).await;
    assert_eq!(world.copies(), [noted], "the copy is kept");
    assert_eq!(std::fs::read(world.master_of(noted)).unwrap(), world.bytes);
    assert_eq!(World::meetings(&app.store), Vec::<Uuid>::new());
    assert_eq!(world.noted(), (1, 1), "still noted, for the next launch");
    quit(app).await;
}

/// The noted copy holds other bytes of the same size: the retry does not
/// admit into it but copies the upload afresh, and the launch adopts the
/// noted copy as a recording with no meeting, without a ledger row. Both
/// copies are kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retry_does_not_admit_into_a_noted_copy_holding_other_bytes() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    let other_bytes = world.corrupt(noted);

    let retried = world.admit(&app).await.unwrap();
    assert_ne!(retried, noted, "copied afresh");
    assert_eq!(world.copies().len(), 2);
    quit(app).await;

    let app = relaunch(&world).await;
    let mut meetings = World::meetings(&app.store);
    meetings.sort();
    let mut both = vec![noted, retried];
    both.sort();
    assert_eq!(meetings, both, "the noted copy is adopted too");
    assert_eq!(
        app.store
            .admitted_meeting(
                world.metadata.recording_id,
                world.metadata.byte_count,
                &world.metadata.sha256
            )
            .unwrap(),
        Some(retried)
    );
    assert_eq!(std::fs::read(world.master_of(noted)).unwrap(), other_bytes);
    assert_eq!(
        std::fs::read(world.master_of(retried)).unwrap(),
        world.bytes
    );
    quit(app).await;
}

/// The retry admits into the noted copy and the ledger answers with
/// another meeting that holds the same bytes: the retry gets that meeting,
/// and the noted copy stays noted for the launch, which removes it once
/// the user deleted that meeting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_noted_copy_the_ledger_answers_as_another_meeting_is_left_to_the_launch() {
    let (world, app) = World::new();
    let (noted, hold) = world.gave_up(&app).await;
    drop(hold);
    let other = world.ledger_holds_the_upload_as_another_meeting(&app.store, false);

    assert_eq!(world.admit(&app).await.unwrap(), other, "the retry");
    assert_eq!(world.copies(), [noted], "the copy is kept");
    assert_eq!(world.noted(), (1, 1), "and still noted");
    app.store.delete_meeting(other).unwrap();
    quit(app).await;

    let app = relaunch(&world).await;
    assert_eq!(
        world.copies(),
        Vec::<Uuid>::new(),
        "removed with its meeting gone"
    );
    assert_eq!(world.noted(), (0, 0));
    quit(app).await;
}
