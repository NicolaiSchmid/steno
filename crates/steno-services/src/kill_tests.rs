//! The recovery of a recording or a delete whose process was killed, with a
//! real kill: each test runs the recorder in a child process (this test
//! binary) held at a gate in its file writer or its delete, kills it, and
//! launches a new app over the same database and audio folder. A kill keeps
//! the page cache, so these show what a crash leaves, not a power loss.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use steno_audio::CaptureError;
use steno_audio::writer::{
    CafStreamWriter, LaneFrames, RecordingFiles, RecordingWriter, RecordingWriting, WavFile,
};
use steno_bridge::{BridgeHost as _, CaptureMode, MeetingIdParams};
use steno_core::{MeetingState, MeetingStateKind, RecordingEndReason, Store, paths::file_url_path};
use steno_host::services::{FileSystem, LeftRecording, Recorder};

use crate::recorder::MakeCaptureSession;
use crate::testing::{
    an_hour_later, app_decoding, app_over_fakes, synthetic_capture, synthetic_capture_through,
};

/// The directory the child records under; unset outside a child.
const DIRECTORY: &str = "STENO_KILL_TEST_DIRECTORY";
/// The child's [`Gate`].
const GATE: &str = "STENO_KILL_TEST_GATE";
/// The test's process id, which the child watches as its parent's.
const PARENT: &str = "STENO_KILL_TEST_PARENT";

/// Where the child's writer waits to be killed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// In the first write, after the writer created the files.
    FirstWrite,
    /// Nowhere: the test kills it once frames are on disk.
    Open,
    /// In the stop, after the writer's finish and before the save.
    AfterFinish,
}

impl Gate {
    fn name(self) -> &'static str {
        match self {
            Gate::FirstWrite => "first-write",
            Gate::Open => "open",
            Gate::AfterFinish => "after-finish",
        }
    }

    fn named(name: &str) -> Self {
        [Gate::FirstWrite, Gate::Open, Gate::AfterFinish]
            .into_iter()
            .find(|gate| gate.name() == name)
            .expect("a known gate")
    }
}

/// The files the child's writer reports through, under its directory.
struct Reports(PathBuf);

impl Reports {
    /// One little-endian `u64` per write: the frames it wrote.
    fn frames(&self) -> PathBuf {
        self.0.join("frames.count")
    }

    /// One byte per sync that returned.
    fn syncs(&self) -> PathBuf {
        self.0.join("syncs.count")
    }

    /// Written when the writer waits at its gate.
    fn gated(&self) -> PathBuf {
        self.0.join("gated")
    }

    /// The writes counted so far.
    fn write_count(&self) -> u64 {
        std::fs::metadata(self.frames()).map_or(0, |metadata| metadata.len() / 8)
    }

    /// The frames the counted writes wrote, and the most one write wrote.
    fn written(&self) -> (u64, u64) {
        let bytes = std::fs::read(self.frames()).unwrap_or_default();
        let writes: Vec<u64> = bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|chunk| u64::from_le_bytes(*chunk))
            .collect();
        (
            writes.iter().sum(),
            writes.iter().copied().max().unwrap_or(0),
        )
    }

    fn sync_count(&self) -> u64 {
        std::fs::metadata(self.syncs()).map_or(0, |metadata| metadata.len())
    }

    fn append(path: &Path, bytes: &[u8]) {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(bytes))
            .expect("the report is written");
    }
}

/// The production writer with the child's gate, counting its writes and
/// its syncs once each has returned.
struct GatedWriter {
    inner: RecordingWriter,
    gate: Gate,
    reports: Reports,
}

impl GatedWriter {
    /// Says so and waits for the kill.
    fn hold(&self) -> ! {
        Reports::append(&self.reports.gated(), b"1");
        loop {
            std::thread::park();
        }
    }
}

impl RecordingWriting for GatedWriter {
    fn files(&self) -> RecordingFiles {
        self.inner.files()
    }

    fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
        if self.gate == Gate::FirstWrite {
            self.hold();
        }
        self.inner.write(frames)?;
        let count = frames.frame_count as u64;
        Reports::append(&self.reports.frames(), &count.to_le_bytes());
        Ok(())
    }

    fn sync(&mut self) -> std::io::Result<()> {
        self.inner.sync()?;
        Reports::append(&self.reports.syncs(), b"1");
        Ok(())
    }

    fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
        let files = self.inner.finish()?;
        if self.gate == Gate::AfterFinish {
            self.hold();
        }
        Ok(files)
    }
}

/// The synthetic capture in real time, written through a [`GatedWriter`].
fn gated_capture(gate: Gate, reports: &Path) -> MakeCaptureSession {
    let reports = reports.to_path_buf();
    synthetic_capture_through(move |inner| {
        Box::new(GatedWriter {
            inner,
            gate,
            reports: Reports(reports.clone()),
        })
    })
}

/// How long a child lives at most: on Windows, which has no parent id to
/// watch, the one guard against a test process that dies before its child.
const CHILD_LIFETIME: Duration = Duration::from_secs(180);

/// Ends this process once its parent is gone or [`CHILD_LIFETIME`] is up,
/// so a killed or failed test leaves no recorder behind.
fn die_with_the_parent() {
    let deadline = Instant::now() + CHILD_LIFETIME;
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

/// The child: records a call until it is killed; at [`Gate::AfterFinish`]
/// it stops once 20 writes are counted. Passes at once outside a child.
#[test]
#[ignore = "the kill tests run it in a child process"]
fn child_records_until_killed() {
    let Ok(directory) = std::env::var(DIRECTORY) else {
        return;
    };
    die_with_the_parent();
    let directory = PathBuf::from(directory);
    let gate = Gate::named(&std::env::var(GATE).unwrap());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let store = Arc::new(Store::open(directory.join("steno.sqlite")).unwrap());
    let app = app_over_fakes(&directory, &store, gated_capture(gate, &directory));
    app.recorder.start(CaptureMode::Call, None);
    let status = app.recorder.status();
    assert_eq!(status.error, None);
    assert!(status.meeting_id.is_some(), "{status:?}");
    if gate == Gate::AfterFinish {
        let reports = Reports(directory.clone());
        while reports.write_count() < 20 {
            std::thread::sleep(Duration::from_millis(10));
        }
        app.recorder.stop();
    }
    loop {
        std::thread::park();
    }
}

/// How long the test waits for the child to reach its point.
const CHILD_PATIENCE: Duration = Duration::from_secs(60);

/// A child process, killed and waited for when this drops.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts the child over `directory` with `gate`.
fn spawn_child(directory: &Path, gate: Gate) -> ChildGuard {
    spawn_test(
        "kill_tests::child_records_until_killed",
        directory,
        gate.name(),
    )
}

/// Starts the ignored test `name` of this binary as the child over
/// `directory` with the gate named `gate`, its output in files there.
fn spawn_test(name: &str, directory: &Path, gate: &str) -> ChildGuard {
    let output = |name: &str| Stdio::from(std::fs::File::create(directory.join(name)).unwrap());
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            name,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(DIRECTORY, directory)
        .env(GATE, gate)
        .env(PARENT, std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(output("child.out"))
        .stderr(output("child.err"))
        .spawn()
        .unwrap();
    ChildGuard(child)
}

/// Waits until `reached` holds while the child runs.
fn wait_until(child: &mut ChildGuard, directory: &Path, what: &str, reached: impl Fn() -> bool) {
    let deadline = Instant::now() + CHILD_PATIENCE;
    while !reached() {
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!(
                "the child exited ({status}) before {what}: {}",
                std::fs::read_to_string(directory.join("child.err")).unwrap_or_default()
            );
        }
        assert!(Instant::now() < deadline, "the child never reached {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits until `reached` holds while the child runs, then kills it.
fn kill_when(child: &mut ChildGuard, directory: &Path, what: &str, reached: impl Fn() -> bool) {
    wait_until(child, directory, what, reached);
    child.0.kill().unwrap();
    child.0.wait().unwrap();
}

/// What the next launch found, once its processing settled.
struct Relaunched {
    launched: steno_core::Meeting,
    settled: steno_core::Meeting,
    asset: Option<steno_core::AudioAsset>,
}

/// The audio folder the child records into.
fn audio_folder(directory: &Path) -> PathBuf {
    directory.join("audio")
}

/// A new app over what the child left, launched as the product launches;
/// every master counts as an old crash's.
async fn relaunch(directory: &Path) -> Relaunched {
    relaunch_in(directory, &audio_folder(directory)).await
}

/// [`relaunch`] with the settings naming `folder` as the audio folder.
async fn relaunch_in(directory: &Path, folder: &Path) -> Relaunched {
    let store = Arc::new(Store::open(directory.join("steno.sqlite")).unwrap());
    let mut app = app_over_fakes(directory, &store, synthetic_capture());
    set_audio_folder(&store, folder);
    app.live_recording_check = an_hour_later();
    let meetings = store.meetings(10, 0).unwrap();
    assert_eq!(meetings.len(), 1, "the child's one meeting");
    assert_eq!(meetings[0].state, MeetingState::Recording);
    let host = Arc::new(app.host().unwrap());
    app.launch(&host);
    app.launch_finished().await;
    let launched = store.meeting(meetings[0].id).unwrap().unwrap();
    app.pipeline.current().wait_until_idle().await;
    let settled = store.meeting(meetings[0].id).unwrap().unwrap();
    let asset = store.asset(meetings[0].id).unwrap();
    Relaunched {
        launched,
        settled,
        asset,
    }
}

/// A kill past the first write: the next launch queues the meeting with
/// the end reason `failed` and the audio up to the last write, and the
/// fakes process it.
fn assert_recovered(relaunched: &Relaunched, frames: (u64, u64)) -> u64 {
    let Relaunched {
        launched,
        settled,
        asset,
    } = relaunched;
    assert!(
        matches!(
            launched.state.kind(),
            MeetingStateKind::Queued | MeetingStateKind::Processing | MeetingStateKind::Ready
        ),
        "{:?}",
        launched.state
    );
    assert_eq!(launched.state.failure_reason(), None);
    assert_eq!(launched.end_reason, Some(RecordingEndReason::Failed));
    assert_eq!(settled.state, MeetingState::Ready);
    let asset = asset.as_ref().expect("the asset");
    let master = file_url_path(&asset.url).unwrap();
    let header = steno_audio::writer::CafHeader::read(&master).unwrap();
    let (counted, largest) = frames;
    assert!(
        (counted..=counted + largest).contains(&header.frame_count),
        "{} frames on disk, {counted} counted",
        header.frame_count
    );
    assert_eq!(launched.duration, header.duration());
    header.frame_count
}

/// Points the settings of `store` at `folder`.
fn set_audio_folder(store: &Store, folder: &Path) {
    let mut settings = store.settings().unwrap();
    settings.audio_folder = steno_core::paths::file_url(folder, true);
    store.save_settings(&settings).unwrap();
}

/// The one meeting the child wrote, read from its database.
fn the_meeting(directory: &Path) -> steno_core::Meeting {
    let store = Store::open(directory.join("steno.sqlite")).unwrap();
    let mut meetings = store.meetings(10, 0).unwrap();
    assert_eq!(meetings.len(), 1, "the child's one meeting");
    meetings.remove(0)
}

/// Killed in the first write: the master holds its header alone, and the
/// next launch fails the meeting. The meeting is listed, never missing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_before_the_first_write_fails_the_meeting() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports(dir.path().to_path_buf());
    let mut child = spawn_child(dir.path(), Gate::FirstWrite);
    kill_when(&mut child, dir.path(), "the first write", || {
        reports.gated().exists()
    });
    assert_eq!(reports.written(), (0, 0));

    let relaunched = relaunch(dir.path()).await;
    assert_eq!(
        relaunched.settled.state,
        MeetingState::Failed {
            reason: Store::INTERRUPTED_RECORDING_REASON.to_owned()
        }
    );
    assert!(relaunched.asset.is_none());
}

/// Killed after ten writes, before any sync.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_after_some_frames_recovers_them() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports(dir.path().to_path_buf());
    let mut child = spawn_child(dir.path(), Gate::Open);
    kill_when(&mut child, dir.path(), "ten writes", || {
        reports.write_count() >= 10
    });
    let written = reports.written();
    let recovered = assert_recovered(&relaunch(dir.path()).await, written);
    assert!(recovered > 0);
}

/// Killed after the writer's first periodic sync: every frame it covered
/// is recovered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_mid_recording_recovers_what_was_written_and_synced() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports(dir.path().to_path_buf());
    let mut child = spawn_child(dir.path(), Gate::Open);
    kill_when(&mut child, dir.path(), "the first sync", || {
        reports.sync_count() >= 1
    });
    let written = reports.written();
    let recovered = assert_recovered(&relaunch(dir.path()).await, written);
    let synced = steno_audio::writer::writer_thread::SYNC_INTERVAL_FRAMES as u64
        * steno_audio::FRAME_SIZE as u64;
    assert!(recovered >= synced, "{recovered} frames, {synced} synced");
}

/// Killed in the stop, after the writer finished and before the save:
/// recovered with the end reason `failed` and every frame written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_between_the_writers_finish_and_the_save_recovers_the_recording() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports(dir.path().to_path_buf());
    let mut child = spawn_child(dir.path(), Gate::AfterFinish);
    kill_when(&mut child, dir.path(), "the stop's gate", || {
        reports.gated().exists()
    });
    let (counted, _) = reports.written();
    // The finish patched every header before the kill.
    let layout =
        steno_core::RecordingLayout::new(&audio_folder(dir.path()), the_meeting(dir.path()).id);
    let master = std::fs::read(layout.master(steno_core::AudioFormat::Caf48kFloat32)).unwrap();
    let at = usize::try_from(CafStreamWriter::DATA_SIZE_OFFSET).unwrap();
    let data_size = i64::from_be_bytes(master[at..at + 8].try_into().unwrap());
    let channels = 2;
    let samples = CafStreamWriter::EDIT_COUNT_SIZE as u64
        + counted * channels * CafStreamWriter::BYTES_PER_SAMPLE as u64;
    assert_eq!(data_size, i64::try_from(samples).unwrap());
    for lane in [steno_core::AudioLane::Mic, steno_core::AudioLane::System] {
        let sidecar = WavFile::read_16k_mono(&layout.sidecar(lane)).unwrap();
        assert_eq!(sidecar.len() as u64, counted / 3, "{}", lane.as_str());
    }

    let recovered = assert_recovered(&relaunch(dir.path()).await, (counted, 0));
    assert_eq!(recovered, counted, "the finished master, whole");
}

/// The child: records, stops once 20 writes are counted, and holds inside
/// the stop's save transaction before the commit until it is killed.
/// Passes at once outside a child.
#[test]
#[ignore = "the kill tests run it in a child process"]
fn child_holds_inside_the_stops_save_until_killed() {
    let Ok(directory) = std::env::var(DIRECTORY) else {
        return;
    };
    die_with_the_parent();
    let directory = PathBuf::from(directory);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let store = Arc::new(Store::open(directory.join("steno.sqlite")).unwrap());
    let reports = Reports(directory.clone());
    {
        let reports = Reports(directory.clone());
        store.probe_commits(move |connection| {
            let assets: i64 = connection
                .query_row("SELECT count(*) FROM audioAsset", [], |row| row.get(0))
                .unwrap();
            if assets > 0 {
                hold_at_the_gate(&reports);
            }
        });
    }
    let app = app_over_fakes(&directory, &store, gated_capture(Gate::Open, &directory));
    app.recorder.start(CaptureMode::Call, None);
    assert_eq!(app.recorder.status().error, None);
    while reports.write_count() < 20 {
        std::thread::sleep(Duration::from_millis(10));
    }
    app.recorder.stop();
    loop {
        std::thread::park();
    }
}

/// Killed inside the stop's save, before its commit: the row is still
/// `recording`, and the next launch recovers the recording whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_inside_the_stops_save_recovers_the_recording() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports(dir.path().to_path_buf());
    let mut child = spawn_test(
        "kill_tests::child_holds_inside_the_stops_save_until_killed",
        dir.path(),
        Gate::Open.name(),
    );
    kill_when(&mut child, dir.path(), "the save's gate", || {
        reports.gated().exists()
    });
    let (counted, _) = reports.written();
    assert_eq!(
        the_meeting(dir.path()).state,
        MeetingState::Recording,
        "the save did not commit"
    );
    let recovered = assert_recovered(&relaunch(dir.path()).await, (counted, 0));
    assert_eq!(recovered, counted, "the finished master, whole");
}

/// The first recording in a newly chosen folder, the setting moved while
/// it runs, then the kill: the next launch recovers the master from the
/// folder the recorder recorded before it wrote the row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_after_the_folder_changed_recovers_from_the_folder_it_started_in() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports(dir.path().to_path_buf());
    let mut child = spawn_child(dir.path(), Gate::Open);
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    wait_until(&mut child, dir.path(), "five writes", || {
        reports.write_count() >= 5
    });
    set_audio_folder(
        &Store::open(dir.path().join("steno.sqlite")).unwrap(),
        &elsewhere,
    );
    kill_when(&mut child, dir.path(), "ten writes", || {
        reports.write_count() >= 10
    });
    let written = reports.written();
    let relaunched = relaunch_in(dir.path(), &elsewhere).await;
    assert_recovered(&relaunched, written);
    let asset = relaunched.asset.unwrap();
    assert_eq!(
        file_url_path(&asset.url)
            .unwrap()
            .parent()
            .unwrap()
            .parent(),
        Some(audio_folder(dir.path()).as_path())
    );
}

/// Where the child's delete waits to be killed, between two of the
/// delete's steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteGate {
    /// The delete's transaction open, a commit probe holding it.
    BeforeCommit,
    /// The rows deleted and committed, the meeting's files not yet removed.
    AfterCommit,
    /// The delete refused, the rows kept, the forgotten entry not yet
    /// recorded again.
    BeforeRestore,
}

impl DeleteGate {
    fn name(self) -> &'static str {
        match self {
            DeleteGate::BeforeCommit => "before-commit",
            DeleteGate::AfterCommit => "after-commit",
            DeleteGate::BeforeRestore => "before-restore",
        }
    }

    fn named(name: &str) -> Self {
        [
            DeleteGate::BeforeCommit,
            DeleteGate::AfterCommit,
            DeleteGate::BeforeRestore,
        ]
        .into_iter()
        .find(|gate| gate.name() == name)
        .expect("a known gate")
    }
}

/// Says the child reached its gate, and waits there for the kill.
fn hold_at_the_gate(reports: &Reports) -> ! {
    Reports::append(&reports.gated(), b"1");
    loop {
        std::thread::park();
    }
}

/// The real file system, held at the first removal.
struct RemovalGate(Reports);

impl FileSystem for RemovalGate {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn remove(&self, _path: &Path) -> steno_core::BoundaryResult<()> {
        hold_at_the_gate(&self.0)
    }
}

/// The capture recorder, held where a refused delete records the
/// forgotten entry again.
struct RestoreGate {
    inner: Arc<crate::recorder::CaptureRecorder>,
    reports: Reports,
}

impl Recorder for RestoreGate {
    fn status(&self) -> steno_host::services::RecorderStatus {
        self.inner.status()
    }
    fn start(&self, mode: CaptureMode, call_app: Option<&str>) {
        self.inner.start(mode, call_app);
    }
    fn hold_starts(&self) -> steno_host::services::StartHold {
        self.inner.hold_starts()
    }
    fn refuse_starts(&self) {
        self.inner.refuse_starts();
    }
    fn stop(&self) {
        self.inner.stop();
    }
    fn toggle(&self) {
        self.inner.toggle();
    }
    fn keep_recording(&self) {
        self.inner.keep_recording();
    }
    fn clear_messages(&self) {
        self.inner.clear_messages();
    }
    fn refresh_permissions(&self) {
        self.inner.refresh_permissions();
    }
    fn remember_audio_folder(&self, folder: &Path) {
        self.inner.remember_audio_folder(folder);
    }
    fn left_recording(&self, meeting_id: uuid::Uuid) -> LeftRecording {
        self.inner.left_recording(meeting_id)
    }
    fn forget_recording(&self, meeting_id: uuid::Uuid) -> std::io::Result<Option<PathBuf>> {
        self.inner.forget_recording(meeting_id)
    }
    fn restore_recording(&self, _meeting_id: uuid::Uuid, _folder: &Path) {
        hold_at_the_gate(&self.reports)
    }
}

/// The file the child writes its meeting's id into.
fn meeting_id_file(directory: &Path) -> PathBuf {
    directory.join("meeting.id")
}

/// The child: records and saves a meeting, then deletes it through the
/// host until it is killed at its [`DeleteGate`]. Passes at once outside a
/// child.
#[test]
#[ignore = "the kill tests run it in a child process"]
fn child_deletes_until_killed() {
    let Ok(directory) = std::env::var(DIRECTORY) else {
        return;
    };
    die_with_the_parent();
    let directory = PathBuf::from(directory);
    let gate = DeleteGate::named(&std::env::var(GATE).unwrap());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let store = Arc::new(Store::open(directory.join("steno.sqlite")).unwrap());
    let mut app = app_over_fakes(&directory, &store, synthetic_capture());
    app.recorder.start(CaptureMode::InPerson, None);
    let meeting_id = app.recorder.status().meeting_id.expect("a recording");
    std::thread::sleep(Duration::from_millis(500));
    app.recorder.stop();
    runtime.block_on(app.pipeline.current().wait_until_idle());
    assert_eq!(
        store.meeting(meeting_id).unwrap().unwrap().state,
        MeetingState::Ready
    );
    assert!(
        crate::audio_folders::recorded(&directory.join("support"))
            .unwrap()
            .contains_key(&meeting_id),
        "the recorder kept the meeting's folder"
    );
    std::fs::write(meeting_id_file(&directory), meeting_id.to_string()).unwrap();

    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    match gate {
        DeleteGate::BeforeCommit => {
            let (armed, reports) = (armed.clone(), Reports(directory.clone()));
            store.probe_commits(move |_| {
                if armed.load(std::sync::atomic::Ordering::SeqCst) {
                    hold_at_the_gate(&reports);
                }
            });
        }
        DeleteGate::AfterCommit => {
            app.services.file_system = Arc::new(RemovalGate(Reports(directory.clone())));
        }
        DeleteGate::BeforeRestore => {
            app.services.recorder = Arc::new(RestoreGate {
                inner: app.recorder.clone(),
                reports: Reports(directory.clone()),
            });
        }
    }
    let confirmed = store.clone();
    let host = app.host().unwrap().with_dialogs(
        Box::new(move |_| {
            if gate == DeleteGate::BeforeRestore {
                confirmed
                    .set_state(meeting_id, MeetingState::Processing, chrono::Utc::now())
                    .unwrap();
            }
            true
        }),
        Box::new(|_| None),
    );
    armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = host.meetings_delete(MeetingIdParams { meeting_id });
    panic!("the delete passed its gate");
}

/// Kills the child at `gate` and launches a new app over what it left; the
/// meeting's id and the meetings that launch lists.
async fn delete_killed_at(
    directory: &Path,
    gate: DeleteGate,
) -> (uuid::Uuid, Vec<steno_core::Meeting>) {
    let reports = Reports(directory.to_path_buf());
    let mut child = spawn_test(
        "kill_tests::child_deletes_until_killed",
        directory,
        gate.name(),
    );
    kill_when(&mut child, directory, "the delete's gate", || {
        reports.gated().exists()
    });
    let meeting_id: uuid::Uuid = std::fs::read_to_string(meeting_id_file(directory))
        .unwrap()
        .parse()
        .unwrap();
    let store = Arc::new(Store::open(directory.join("steno.sqlite")).unwrap());
    let mut app = app_over_fakes(directory, &store, synthetic_capture());
    app.live_recording_check = an_hour_later();
    let host = Arc::new(app.host().unwrap());
    app.launch(&host);
    app.launch_finished().await;
    app.pipeline.current().wait_until_idle().await;
    (meeting_id, store.meetings(10, 0).unwrap())
}

/// The master the child recorded for `meeting_id`.
fn master_of(directory: &Path, meeting_id: uuid::Uuid) -> PathBuf {
    steno_core::RecordingLayout::new(&audio_folder(directory), meeting_id)
        .master(steno_core::AudioFormat::Caf48kFloat32)
}

/// Asserts that the launch lists the meeting once, in `state`, with its
/// recording on disk.
fn assert_listed_with_its_recording(
    directory: &Path,
    meeting_id: uuid::Uuid,
    listed: &[steno_core::Meeting],
    state: &MeetingState,
) {
    assert_eq!(
        listed.iter().map(|meeting| meeting.id).collect::<Vec<_>>(),
        [meeting_id],
        "the meeting, listed once"
    );
    assert_eq!(&listed[0].state, state);
    let store = Store::open(directory.join("steno.sqlite")).unwrap();
    let asset = store.asset(meeting_id).unwrap().expect("its recording");
    assert_eq!(
        file_url_path(&asset.url),
        Some(master_of(directory, meeting_id))
    );
    assert!(
        master_of(directory, meeting_id).exists(),
        "the master on disk"
    );
}

/// Killed before the delete's rows went: the next launch lists the meeting
/// with its recording, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_after_the_entry_is_forgotten_keeps_the_meeting() {
    let dir = tempfile::tempdir().unwrap();
    let (meeting_id, listed) = delete_killed_at(dir.path(), DeleteGate::BeforeCommit).await;
    assert_listed_with_its_recording(dir.path(), meeting_id, &listed, &MeetingState::Ready);
}

/// Killed after the delete committed and before its files went: the next
/// launch does not list the meeting and adopts no master.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_after_the_delete_commits_never_brings_the_meeting_back() {
    let dir = tempfile::tempdir().unwrap();
    let (meeting_id, listed) = delete_killed_at(dir.path(), DeleteGate::AfterCommit).await;
    assert!(
        master_of(dir.path(), meeting_id).exists(),
        "the master was left"
    );
    assert_eq!(listed, Vec::new(), "nothing listed, nothing adopted");
}

/// A refused delete killed before the forgotten entry was recorded again:
/// the next launch lists the kept meeting with its recording, once, failed
/// as one the kill ended mid-run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_before_a_kept_meetings_entry_is_restored_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let (meeting_id, listed) = delete_killed_at(dir.path(), DeleteGate::BeforeRestore).await;
    let failed = MeetingState::Failed {
        reason: steno_pipeline::CLOSED_WHILE_PROCESSING.to_owned(),
    };
    assert_listed_with_its_recording(dir.path(), meeting_id, &listed, &failed);
}

/// The child's decoder for the reading test: a decode writes `reading` and
/// waits for the kill.
struct DecodeUntilKilled {
    reading: PathBuf,
}

#[steno_core::async_trait]
impl steno_core::AudioDecoder for DecodeUntilKilled {
    async fn decode(
        &self,
        _: &steno_core::AudioAsset,
        _: steno_core::AudioLane,
    ) -> steno_core::BoundaryResult<steno_core::AudioBuffer16k> {
        Reports::append(&self.reading, b"1");
        loop {
            std::thread::park();
        }
    }

    fn mixdown_format(&self) -> steno_core::AudioFormat {
        steno_core::AudioFormat::Wav16kInt16
    }

    async fn mixdown(
        &self,
        _: &steno_core::AudioAsset,
        _: &Path,
    ) -> steno_core::BoundaryResult<()> {
        Ok(())
    }
}

/// The child: processes the asset in `bad` and is killed in the decode.
/// Passes at once outside a child.
#[test]
#[ignore = "the kill tests run it in a child process"]
fn child_decodes_until_killed() {
    let Ok(directory) = std::env::var(DIRECTORY) else {
        return;
    };
    die_with_the_parent();
    let directory = PathBuf::from(directory);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let store = Arc::new(Store::open(directory.join("steno.sqlite")).unwrap());
    let decoder = Arc::new(DecodeUntilKilled {
        reading: directory.join("reading"),
    });
    let app = app_decoding(&directory, &store, decoder);
    let asset_id: uuid::Uuid = std::fs::read_to_string(directory.join("bad"))
        .unwrap()
        .parse()
        .unwrap();
    let _ = runtime.block_on(app.pipeline.current().process(asset_id));
    unreachable!("the decode never returns");
}

/// A queued call meeting with its recording under `directory/audio`.
fn queued_call(store: &Store, directory: &Path) -> (steno_core::Meeting, steno_core::AudioAsset) {
    let mut meeting = steno_core::testing::sample_data::meeting();
    meeting.id = uuid::Uuid::new_v4();
    meeting.state = MeetingState::Queued;
    let asset = steno_pipeline::fixtures::two_lane_call(
        &audio_folder(directory),
        meeting.id,
        steno_core::AudioRetention::DeleteAfterProcessing,
    )
    .unwrap();
    store.save_meeting_with_asset(&meeting, &asset).unwrap();
    (meeting, asset)
}

/// A child that processes `bad` and is killed in its decode.
fn killed_in_the_decode(dir: &Path, bad: &steno_core::AudioAsset) {
    std::fs::write(dir.join("bad"), bad.id.to_string()).unwrap();
    let reading = dir.join("reading");
    let mut child = spawn_test(
        "kill_tests::child_decodes_until_killed",
        dir,
        Gate::Open.name(),
    );
    kill_when(&mut child, dir, "the decode", || reading.exists());
}

/// The next launch over the database the children used.
async fn launched_after(dir: &Path) -> (Arc<Store>, crate::App) {
    let store = Arc::new(Store::open(dir.join("steno.sqlite")).unwrap());
    let app = app_over_fakes(dir, &store, synthetic_capture());
    (store, launched(app).await)
}

/// `app` launched, its queued meetings processed.
async fn launched(app: crate::App) -> crate::App {
    let host = Arc::new(app.host().unwrap());
    app.launch(&host);
    app.launch_finished().await;
    app.pipeline.current().wait_until_idle().await;
    app
}

/// A process killed while it reads a recording leaves that meeting
/// `processing`: the next launch fails it, keeps its files and still
/// processes the other queued meeting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_killed_fails_its_meeting_at_the_next_launch() {
    let dir = tempfile::tempdir().unwrap();
    let (bad, bad_asset, good) = {
        let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
        let (bad, bad_asset) = queued_call(&store, dir.path());
        let (good, _) = queued_call(&store, dir.path());
        (bad, bad_asset, good)
    };
    killed_in_the_decode(dir.path(), &bad_asset);

    let (store, _app) = launched_after(dir.path()).await;
    assert_eq!(
        store.meeting(bad.id).unwrap().unwrap().state,
        MeetingState::Failed {
            reason: steno_pipeline::CLOSED_WHILE_PROCESSING.to_owned()
        }
    );
    assert!(file_url_path(&bad_asset.url).unwrap().exists());
    assert_eq!(
        store.meeting(good.id).unwrap().unwrap().state,
        MeetingState::Ready
    );
}
