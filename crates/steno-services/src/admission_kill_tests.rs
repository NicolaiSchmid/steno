//! The noted copy of a phone admission at the edges: a kill at each step
//! of the admission and of the launch's adoption, a retry that races the
//! launch, and a record of uploads that is refused or does not parse
//! (`.plans/2026-10-07-stable-promotion.md`, "P40 follow-up: the copy an
//! admission left is one meeting").
//!
//! The kills are real: a phone admission, or the launch that adopts the
//! noted copy, runs in a child process (this test binary, running
//! [`child_admits_until_killed`] or [`child_launches_until_killed`]) and is
//! killed at one gate, a test seam the product does not have ([`gate`]),
//! or inside a commit, in the store's commit probe. A new app then
//! launches over the same directory and the phone retries. Each ends with
//! one meeting, one ledger row answering the phone with it, the upload's
//! bytes in its master, and nothing left noted. The gates (all
//! `#[cfg(test)]`): `after-upload-note` and `after-folder-note` in
//! `AdmissionRecord::copying`, `before-settled` in
//! `AdmissionRecord::settled`, and `after-adoption-commit` in the
//! recovery's `adopt`, once the adoption's commit wrote and before the
//! enqueue.
//!
//! The races hold the retry after its check that the noted copy has no
//! meeting row (`steno_pipeline::intake::seams`, the pipeline's `testing`
//! feature) and run a launch there. Rust only.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use steno_core::{PairedDevice, RecordingMetadata, Store};
use uuid::Uuid;

use crate::App;
use crate::admission_copy_tests::{World, quit, relaunch};
use crate::testing::{HANG_GUARD, an_hour_later, app_over_fakes, database_in, synthetic_capture};

/// The directory the child works in; unset outside a child.
const DIRECTORY: &str = "STENO_ADMISSION_KILL_DIRECTORY";
/// The child's gate.
const GATE: &str = "STENO_ADMISSION_KILL_GATE";
/// The test's process id, which the child watches as its parent's.
const PARENT: &str = "STENO_ADMISSION_KILL_PARENT";
/// The gate inside a commit that writes a meeting and a ledger row.
const IN_LEDGER_COMMIT: &str = "in-ledger-commit";
/// What the child needs of the world: the upload and the phone.
const HANDED: &str = "handed.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct Handed {
    metadata: RecordingMetadata,
    device: PairedDevice,
}

/// The seam: holds this thread for the kill when the child's gate is
/// `point`. Does nothing outside a child.
pub(crate) fn gate(point: &str) {
    if std::env::var(GATE).as_deref() == Ok(point)
        && let Ok(directory) = std::env::var(DIRECTORY)
    {
        hold(Path::new(&directory));
    }
}

fn hold(directory: &Path) -> ! {
    std::fs::write(directory.join("gated"), b"1").unwrap();
    loop {
        std::thread::park();
    }
}

/// Ends the child once its parent is gone or a minute and a half passed.
fn die_with_the_parent() {
    let deadline = Instant::now() + Duration::from_secs(90);
    #[cfg(unix)]
    let parent: u32 = std::env::var(PARENT).unwrap().parse().unwrap();
    std::thread::spawn(move || {
        loop {
            #[cfg(unix)]
            if std::os::unix::process::parent_id() != parent {
                std::process::exit(2);
            }
            if Instant::now() >= deadline {
                std::process::exit(3);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}

/// In the child: holds inside the first commit that writes a meeting with
/// a ledger row (the admission's, or the launch's adoption), before COMMIT.
fn hold_in_the_ledger_commit(store: &Store, directory: &Path) {
    let directory = directory.to_path_buf();
    store.probe_commits(move |connection| {
        let count = |table: &str| -> i64 {
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        if count("meeting") > 0 && count("handoverAdmission") > 0 {
            hold(&directory);
        }
    });
}

/// The child's directory and store, the probe set when its gate asks.
fn child_world() -> Option<(PathBuf, Arc<Store>, tokio::runtime::Runtime)> {
    let directory = PathBuf::from(std::env::var(DIRECTORY).ok()?);
    die_with_the_parent();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let store = Arc::new(Store::open(database_in(&directory)).unwrap());
    if std::env::var(GATE).unwrap() == IN_LEDGER_COMMIT {
        hold_in_the_ledger_commit(&store, &directory);
    }
    Some((directory, store, runtime))
}

/// The child: the phone's `complete` through the app's intake, held at its
/// gate until killed. Passes at once outside a child.
#[test]
#[ignore = "the admission kill tests run it in a child process"]
fn child_admits_until_killed() {
    let Some((directory, store, runtime)) = child_world() else {
        return;
    };
    runtime.block_on(async {
        let app = app_over_fakes(&directory, &store, synthetic_capture());
        let handed: Handed =
            serde_json::from_slice(&std::fs::read(directory.join(HANDED)).unwrap()).unwrap();
        let intake = crate::app::handover_intake(
            app.store.clone(),
            app.pipeline.clone(),
            app.zone,
            &directory.join("support"),
        );
        let outcome = steno_core::HandoverIntake::admit(
            &intake,
            &directory.join("inbox").join("upload.wav"),
            &handed.metadata,
            &handed.device,
        )
        .await;
        std::fs::write(directory.join("returned"), format!("{outcome:?}")).unwrap();
        std::future::pending::<()>().await;
    });
}

/// The child: a launch as the product launches, held at its gate until
/// killed. Passes at once outside a child.
#[test]
#[ignore = "the admission kill tests run it in a child process"]
fn child_launches_until_killed() {
    let Some((directory, store, runtime)) = child_world() else {
        return;
    };
    runtime.block_on(async {
        let mut app = app_over_fakes(&directory, &store, synthetic_capture());
        app.live_recording_check = an_hour_later();
        let host = Arc::new(app.host().unwrap());
        app.launch(&host);
        app.launch_finished().await;
        std::fs::write(directory.join("returned"), b"launched").unwrap();
        std::future::pending::<()>().await;
    });
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs the child test `name` over `world` with `point`, waits for its
/// gate and kills it there.
fn killed_at(world: &World, name: &str, point: &str) {
    let directory = world.dir.path();
    std::fs::write(
        directory.join(HANDED),
        serde_json::to_vec(&Handed {
            metadata: world.metadata.clone(),
            device: world.device.clone(),
        })
        .unwrap(),
    )
    .unwrap();
    let output = |file: &str| Stdio::from(std::fs::File::create(directory.join(file)).unwrap());
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args([
                &format!("admission_kill_tests::{name}"),
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(DIRECTORY, directory)
            .env(GATE, point)
            .env(PARENT, std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(output("child.out"))
            .stderr(output("child.err"))
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while !directory.join("gated").exists() {
        let err = || std::fs::read_to_string(directory.join("child.err")).unwrap_or_default();
        assert!(
            !directory.join("returned").exists(),
            "the child passed {point}: {}",
            std::fs::read_to_string(directory.join("returned")).unwrap()
        );
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!("the child exited ({status}) before {point}: {}", err());
        }
        assert!(Instant::now() < deadline, "the child never reached {point}");
        std::thread::sleep(Duration::from_millis(5));
    }
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    std::fs::remove_file(directory.join("gated")).unwrap();
}

/// A fresh admission killed at `point`; then a relaunch and the retry.
/// Returns the noted entries at the kill and after the retry.
async fn fresh_admission_killed_at(point: &str) -> ((usize, usize), (usize, usize)) {
    let (world, app) = World::new();
    quit(app).await;
    killed_at(&world, "child_admits_until_killed", point);
    let at_kill = world.noted();
    let app = relaunch(&world).await;
    let retried = world.admit(&app).await.unwrap();
    world.assert_one_meeting(&app.store, retried);
    quit(app).await;
    let app = relaunch(&world).await;
    world.assert_one_meeting(&app.store, retried);
    let after = world.noted();
    quit(app).await;
    eprintln!("{point}: noted at the kill {at_kill:?}, after {after:?}");
    (at_kill, after)
}

/// (a) Killed after the upload's note, before the folder's: no copy, the
/// retry copies afresh, and the next launch forgets the upload with no
/// folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_after_the_upload_note_is_one_meeting() {
    let (at_kill, after) = fresh_admission_killed_at("after-upload-note").await;
    assert_eq!(at_kill, (0, 1));
    assert_eq!(after.0, 0, "no folder left noted");
    assert_eq!(after.1, 0, "no upload left noted");
}

/// (b1) Killed after the folder's note, before the copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_after_the_folder_note_is_one_meeting() {
    let (at_kill, after) = fresh_admission_killed_at("after-folder-note").await;
    assert_eq!(at_kill, (1, 1));
    assert_eq!(after, (0, 0));
}

/// (b2) Killed inside the admission's commit, the copy whole: the launch
/// adopts it with the ledger row and the retry is answered with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_inside_the_admissions_commit_is_one_meeting() {
    let (at_kill, after) = fresh_admission_killed_at(IN_LEDGER_COMMIT).await;
    assert_eq!(at_kill, (1, 1));
    assert_eq!(after, (0, 0));
}

/// (e) Killed after the admission's commit, before its settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_after_the_admissions_commit_is_one_meeting() {
    let (at_kill, after) = fresh_admission_killed_at("before-settled").await;
    assert_eq!(at_kill, (1, 1));
    assert_eq!(after, (0, 0));
}

/// An admission that gave up, then a launch killed at `point`; then a
/// relaunch and the retry: one meeting, the noted one.
async fn adoption_killed_at(point: &str) -> ((usize, usize), (usize, usize)) {
    let (world, app) = World::new();
    let (noted_id, hold) = world.gave_up(&app).await;
    drop(hold);
    quit(app).await;
    killed_at(&world, "child_launches_until_killed", point);
    let at_kill = world.noted();
    let app = relaunch(&world).await;
    world.assert_one_meeting(&app.store, noted_id);
    assert_eq!(world.admit(&app).await.unwrap(), noted_id, "the retry");
    world.assert_one_meeting(&app.store, noted_id);
    quit(app).await;
    let app = relaunch(&world).await;
    world.assert_one_meeting(&app.store, noted_id);
    let after = world.noted();
    quit(app).await;
    eprintln!("{point}: noted at the kill {at_kill:?}, after {after:?}");
    (at_kill, after)
}

/// (c) Killed inside the adoption's transaction, before COMMIT.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_inside_the_adoptions_commit_is_one_meeting() {
    let (at_kill, after) = adoption_killed_at(IN_LEDGER_COMMIT).await;
    assert_eq!(at_kill, (1, 1));
    assert_eq!(after, (0, 0));
}

/// (d) Killed between the adoption's commit and the forget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_after_the_adoptions_commit_is_one_meeting() {
    let (at_kill, after) = adoption_killed_at("after-adoption-commit").await;
    assert_eq!(at_kill, (1, 1));
    assert_eq!(after, (0, 0));
}

/// The folder's note fails on every retry (the record of folders is a
/// folder): each refused retry forgets its upload again, so the record of
/// uploads does not grow, and a retry once the note works is one meeting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_folder_note_leaves_no_upload_entry() {
    let (world, app) = World::new();
    std::fs::create_dir_all(world.support().join(crate::audio_folders::RECORDED_FILE)).unwrap();
    for _ in 0..10 {
        world.admit(&app).await.unwrap_err();
    }
    let entries = crate::audio_folders::uploads(&world.support())
        .unwrap()
        .len();
    std::fs::remove_dir(world.support().join(crate::audio_folders::RECORDED_FILE)).unwrap();
    let retried = world.admit(&app).await.unwrap();
    world.assert_one_meeting(&app.store, retried);
    quit(app).await;
    assert_eq!(entries, 0, "a refused admission notes no upload");
}

/// Holds the retry of `noted_id` after its check that the noted copy has
/// no meeting row, until the test runs `between`: the order of a launch
/// that runs while the retry is under way.
async fn retry_held_after_the_reuse_check<T>(
    world: &Arc<World>,
    app: App,
    noted_id: Uuid,
    between: impl AsyncFnOnce() -> T,
) -> (steno_core::protocols::BoundaryResult<Uuid>, App, T) {
    let checked = Arc::new(std::sync::Barrier::new(2));
    let go = Arc::new(std::sync::Barrier::new(2));
    {
        let (checked, go) = (checked.clone(), go.clone());
        steno_pipeline::intake::seams::hold_after_the_reuse_check(move |meeting_id| {
            if meeting_id == noted_id {
                checked.wait();
                go.wait();
            }
        });
    }
    let retry = {
        let world = world.clone();
        tokio::spawn(async move {
            let outcome = world.admit(&app).await;
            (outcome, app)
        })
    };
    tokio::task::spawn_blocking(move || checked.wait())
        .await
        .unwrap();
    let between = between().await;
    tokio::task::spawn_blocking(move || go.wait())
        .await
        .unwrap();
    let (outcome, app) = retry.await.unwrap();
    (outcome, app, between)
}

/// The retry passed its check that the noted copy has no meeting row,
/// then the launch adopts the copy without the ledger row (its read of the
/// noted uploads found none: the file moved away for it), then the retry
/// commits into the same meeting id. The commit is refused: one meeting,
/// one asset row, kept forever as the adoption saved it, and the audio
/// kept, so no sweep removes a master another row keeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retry_racing_a_plain_adoption_of_its_copy_writes_no_second_asset() {
    let (world, app) = World::new();
    let mut settings = app.store.settings().unwrap();
    settings.default_retention = steno_core::AudioRetention::DeleteAfterProcessing;
    app.store.save_settings(&settings).unwrap();
    let (noted_id, hold) = world.gave_up(&app).await;
    drop(hold);
    let uploads_file = world.support().join(crate::audio_folders::UPLOADS_FILE);
    let aside = world.dir.path().join("uploads.aside");
    let world = Arc::new(world);
    let (outcome, app, launched) =
        retry_held_after_the_reuse_check(&world, app, noted_id, async || {
            std::fs::rename(&uploads_file, &aside).unwrap();
            let launched = relaunch(&world).await;
            assert!(
                launched.store.meeting(noted_id).unwrap().is_some(),
                "adopted without the ledger row"
            );
            std::fs::rename(&aside, &uploads_file).unwrap();
            launched
        })
        .await;
    outcome.expect_err("the commit into the adopted meeting is refused");
    for app in [&launched, &app] {
        tokio::time::timeout(HANG_GUARD, app.pipeline.current().wait_until_idle())
            .await
            .unwrap();
    }
    assert_eq!(World::meetings(&launched.store), [noted_id], "one meeting");
    let assets = launched.store.assets().unwrap();
    assert_eq!(assets.len(), 1, "one asset row for the master: {assets:?}");
    assert_eq!(assets[0].retention, steno_core::AudioRetention::KeepForever);
    let master = steno_core::paths::file_url_path(&assets[0].url).unwrap();
    assert_eq!(
        std::fs::read(master).unwrap(),
        world.bytes,
        "the audio is kept"
    );
    quit(launched).await;
    quit(app).await;
}

/// The launch and the retry at once: the retry passed its check of the
/// noted copy, then the launch adopts the copy with the ledger row, then
/// the retry commits. Its commit finds the ledger's row and writes
/// nothing: one meeting, one asset, and the retry is answered with the
/// adopted meeting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retry_that_loses_to_the_launchs_adoption_writes_nothing() {
    let (world, app) = World::new();
    let (noted_id, hold) = world.gave_up(&app).await;
    drop(hold);
    let world = Arc::new(world);
    let (outcome, app, launched) =
        retry_held_after_the_reuse_check(&world, app, noted_id, async || {
            let launched = relaunch(&world).await;
            world.assert_one_meeting(&launched.store, noted_id);
            launched
        })
        .await;
    assert_eq!(
        outcome.unwrap(),
        noted_id,
        "the retry gets the adopted meeting"
    );
    for app in [&launched, &app] {
        tokio::time::timeout(HANG_GUARD, app.pipeline.current().wait_until_idle())
            .await
            .unwrap();
    }
    world.assert_one_meeting(&launched.store, noted_id);
    assert_eq!(launched.store.assets().unwrap().len(), 1, "one asset");
    quit(launched).await;
    quit(app).await;
}

/// An `admission-uploads.json` that does not parse is set aside, its
/// bytes kept beside it, and the next note starts it afresh.
#[test]
fn an_unparsable_uploads_record_is_set_aside() {
    let dir = tempfile::tempdir().unwrap();
    let support = dir.path().join("support");
    std::fs::create_dir_all(&support).unwrap();
    let garbage = b"{\"not\": json".to_vec();
    std::fs::write(support.join(crate::audio_folders::UPLOADS_FILE), &garbage).unwrap();
    let folders = crate::audio_folders::AdmissionRecord {
        support_directory: support.clone(),
    };
    let upload = steno_pipeline::NotedUpload {
        recording_id: uuid::Uuid::new_v4(),
        byte_count: 3,
        sha256: vec![7; 32],
    };
    let meeting_id = uuid::Uuid::new_v4();
    steno_pipeline::AdmissionFolders::copying(
        &folders,
        meeting_id,
        &dir.path().join("audio"),
        &upload,
    )
    .unwrap();
    let aside: Vec<Vec<u8>> = std::fs::read_dir(&support)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("admission-uploads.json.corrupt-")
        })
        .map(|entry| std::fs::read(entry.path()).unwrap())
        .collect();
    assert_eq!(aside, [garbage], "the unparsable record is kept beside");
    assert_eq!(
        crate::audio_folders::uploads(&support)
            .unwrap()
            .get(&meeting_id),
        Some(&upload)
    );
}
