//! The host's `Recorder` over the capture session and the Mac recording
//! intake. Swift: `apps/macos/Steno/Recording/RecordingController.swift`.
//! The calendar lookup, the auto-stop after a call ends and the detection
//! prompt are WP5's recorder policy (the plan's parity list); the status
//! carries what the capture session reports.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use chrono::{FixedOffset, Utc};
use steno_audio::{
    CaptureConfiguration, CaptureSession, CaptureStatistics, FRAMES_PER_SECOND,
    LaneLevels as AudioLevels,
};
use steno_bridge::{CaptureMode, PermissionKind, RecordingState};
use steno_core::{MeetingSource, RecordingEndReason, Store};
use steno_host::services::{LaneLevels, Permissions, Recorder, RecorderStatus, SpeechModels};
use steno_host::speech::ModelAsset;
use steno_pipeline::{LocalRecordingIntake, RecordingResult};
use steno_speech::SpeechRuntime;
use uuid::Uuid;

use crate::block_on;
use crate::pipeline::CurrentPipeline;

/// Builds a capture session for a configuration; the product passes
/// `CaptureSession::new`, tests a synthetic backend.
pub type MakeCaptureSession =
    Arc<dyn Fn(CaptureConfiguration) -> Result<CaptureSession, String> + Send + Sync>;

struct Active {
    session: Arc<CaptureSession>,
    meeting_id: Uuid,
    mode: CaptureMode,
    /// The latest lane levels, written by the forwarding thread.
    levels: Arc<Mutex<Option<LaneLevels>>>,
    /// The forwarding thread, joined once the session is dropped.
    level_thread: JoinHandle<()>,
}

struct Inner {
    status: RecorderStatus,
    active: Option<Active>,
    /// Set by [`CaptureRecorder::stop_for_quit`]: the app is ending, so
    /// no recording starts any more.
    quitting: bool,
}

/// dBFS to the `0...1` RMS the bridge carries.
fn linear(db: f32) -> f64 {
    10f64.powf(f64::from(db) / 20.0).clamp(0.0, 1.0)
}

fn levels(levels: &AudioLevels) -> LaneLevels {
    LaneLevels {
        mic: linear(levels.mic.rms),
        system: levels.system.as_ref().map(|lane| linear(lane.rms)),
    }
}

pub struct CaptureRecorder {
    store: Arc<Store>,
    pipeline: Arc<CurrentPipeline>,
    make_session: MakeCaptureSession,
    permissions: Arc<dyn Permissions>,
    /// Whether the models are on disk, so a warm-up never downloads.
    speech_models: Arc<dyn SpeechModels>,
    zone: FixedOffset,
    runtime: tokio::runtime::Handle,
    inner: Mutex<Inner>,
    /// Signalled with every status change, for [`Self::settle`].
    changes: Condvar,
    /// Called after every status change so the host republishes.
    changed: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl CaptureRecorder {
    #[must_use]
    pub fn new(
        store: Arc<Store>,
        pipeline: Arc<CurrentPipeline>,
        make_session: MakeCaptureSession,
        permissions: Arc<dyn Permissions>,
        speech_models: Arc<dyn SpeechModels>,
        zone: FixedOffset,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        CaptureRecorder {
            store,
            pipeline,
            make_session,
            permissions,
            speech_models,
            zone,
            runtime,
            inner: Mutex::new(Inner {
                status: RecorderStatus::idle(),
                active: None,
                quitting: false,
            }),
            changes: Condvar::new(),
            changed: Mutex::new(None),
        }
    }

    /// The hook the app wires to `Host::recorder_changed`.
    pub fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self
            .changed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }

    fn hook(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.changed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn notify(&self) {
        self.changes.notify_all();
        if let Some(hook) = self.hook() {
            hook();
        }
    }

    /// Waits until no start or stop is in progress and hands back the
    /// lock, the recorder `Idle` or `Recording` under it. Swift:
    /// `RecordingController.awaitSettled`.
    fn settle(&self) -> MutexGuard<'_, Inner> {
        self.changes
            .wait_while(self.inner(), |inner| {
                matches!(
                    inner.status.state,
                    RecordingState::Starting | RecordingState::Stopping
                )
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn intake(&self) -> LocalRecordingIntake {
        LocalRecordingIntake::over(self.store.clone(), self.pipeline.current(), self.zone)
    }

    /// Loads the models processing needs in the background while the
    /// recording runs, so processing does not wait for the cold load; only
    /// when the models of the current pipeline's speech engine and the
    /// diarizer's are installed, so it never starts a download. The engine
    /// is the one the pipeline was built with ([`BuiltEngine`]), not the id
    /// stored now (Rust only: Swift asked about the stored id): after a
    /// failed reload, or an engine the Swift app saved meanwhile, the two
    /// differ. An engine in this process (`CoreML` on
    /// the Mac) is loaded with the diarizer, as Swift did. Where the speech
    /// sidecar runs the engine ([`SpeechRuntime`]), only the diarizer is:
    /// the child would hold its 2.2 GB through the whole recording, outside
    /// any job's claim, so the job starts it instead. Swift:
    /// `AppEnvironment.warmUpPipelineIfModelsInstalled`, called when a
    /// recording starts.
    ///
    /// [`BuiltEngine`]: crate::pipeline::BuiltEngine
    fn warm_up_if_installed(&self) {
        let (pipeline, engine) = self.pipeline.current_with_engine();
        if !(self.speech_models.engine_installed(&engine.engine_id)
            && self.speech_models.is_installed(ModelAsset::OfflineDiarizer))
        {
            return;
        }
        let in_process = engine.runtime == SpeechRuntime::CoreMlInProcess;
        self.runtime.spawn(async move {
            let warmed = if in_process {
                pipeline.warm_up().await
            } else {
                pipeline.warm_up_diarizer().await
            };
            if let Err(failure) = warmed {
                tracing::debug!(%failure, "warm-up failed; processing loads the models");
            }
        });
    }

    /// Starts the session and the meeting.
    fn start_inner(&self, mode: CaptureMode, call_app: Option<&str>) -> Result<(), String> {
        let settings = self.store.settings().map_err(|e| e.to_string())?;
        let audio_folder = steno_core::paths::file_url_path(&settings.audio_folder)
            .ok_or_else(|| format!("audio folder is not a file URL: {}", settings.audio_folder))?;
        let audio_mode = match mode {
            CaptureMode::Call => steno_audio::CaptureMode::Call,
            CaptureMode::InPerson => steno_audio::CaptureMode::InPerson,
        };
        let mut configuration = CaptureConfiguration::new(audio_mode, audio_folder);
        configuration
            .input_device_uid
            .clone_from(&settings.input_device_uid);
        let session = Arc::new((self.make_session)(configuration)?);
        let started_at = Utc::now();
        let intake = self.intake();
        let meeting = intake
            .begin(
                match mode {
                    CaptureMode::Call => MeetingSource::MacCall,
                    CaptureMode::InPerson => MeetingSource::MacInPerson,
                },
                None,
                None,
                &[],
                started_at,
            )
            .map_err(|e| e.to_string())?;
        let levels_receiver = session.levels();
        if let Err(error) = session.start(meeting.id) {
            let _ = intake.fail(meeting.id, &format!("Recording could not start: {error}"));
            return Err(error.to_string());
        }
        let meeting_id = meeting.id;
        let shared = Arc::new(Mutex::new(None::<LaneLevels>));
        // Levels arrive on a std channel at 10 Hz; a thread forwards them
        // into `Active::levels` and the host republishes `recording`. It
        // is spawned before `Recording` is visible, so the stop that takes
        // the session always takes the thread with it.
        let hook = self.hook();
        let level_thread = {
            let shared = shared.clone();
            std::thread::spawn(move || {
                while let Ok(update) = levels_receiver.recv() {
                    *shared
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(levels(&update));
                    if let Some(hook) = &hook {
                        hook();
                    }
                }
            })
        };
        {
            let mut inner = self.inner();
            inner.status.state = RecordingState::Recording;
            inner.status.started_at = Some(started_at);
            inner.status.mode = Some(mode);
            inner.status.call_app = call_app.map(str::to_owned);
            inner.status.meeting_id = Some(meeting_id);
            inner.status.levels = None;
            inner.status.error = None;
            inner.status.warning = None;
            inner.active = Some(Active {
                session,
                meeting_id,
                mode,
                levels: shared,
                level_thread,
            });
        }
        Ok(())
    }

    /// Quitting: once a start or a stop in progress has settled, a
    /// recording is stopped with the `quit` end reason and saved (the
    /// asset written and the meeting enqueued) before this returns; a
    /// stop already under way is waited for instead, so its reason stands.
    /// No recording starts afterwards. Swift: `awaitSettled()` and then
    /// `stop(reason: .quit)` in `AppController.shutdown`.
    pub fn stop_for_quit(&self) {
        let active = {
            let mut inner = self.settle();
            inner.quitting = true;
            Self::begin_stop(&mut inner)
        };
        if let Some(active) = active {
            self.finish_stop(active, RecordingEndReason::Quit);
        }
    }

    /// Takes the session of the recording in progress and marks the
    /// recorder `Stopping`; none when nothing records.
    fn begin_stop(inner: &mut Inner) -> Option<Active> {
        let active = inner.active.take()?;
        inner.status.state = RecordingState::Stopping;
        Some(active)
    }

    /// Stops the session `begin_stop` took and saves the recording, then
    /// leaves the recorder `Idle` with the outcome's message; the host
    /// hears of `Stopping` and of `Idle`.
    fn finish_stop(&self, active: Active, reason: RecordingEndReason) {
        self.notify();
        let intake = self.intake();
        let outcome = match active.session.stop() {
            Ok(result) => {
                log_dropped_frames(active.meeting_id, &result.statistics);
                let duration = result.statistics.duration;
                let statistics = result.statistics.clone();
                let completed = block_on(
                    &self.runtime,
                    intake.complete(
                        active.meeting_id,
                        RecordingResult {
                            asset: result.asset,
                            duration,
                            end_reason: reason,
                        },
                        None,
                    ),
                );
                match completed {
                    Ok(_) => Ok(recording_warning(active.mode, &statistics)),
                    Err(error) => Err(format!("Recording could not be saved: {error}")),
                }
            }
            Err(error) => {
                let message = format!("Recording could not be saved: {error}");
                let _ = intake.fail(active.meeting_id, &message);
                Err(message)
            }
        };
        drop(active.session);
        let _ = active.level_thread.join();
        let mut inner = self.inner();
        inner.status.state = RecordingState::Idle;
        inner.status.started_at = None;
        inner.status.mode = None;
        inner.status.call_app = None;
        inner.status.meeting_id = None;
        inner.status.levels = None;
        inner.status.auto_stop = None;
        match outcome {
            Ok(warning) => inner.status.warning = warning,
            Err(error) => inner.status.error = Some(error),
        }
        drop(inner);
        self.notify();
    }
}

impl Recorder for CaptureRecorder {
    fn status(&self) -> RecorderStatus {
        let inner = self.inner();
        let mut status = inner.status.clone();
        if status.state == RecordingState::Recording
            && let Some(active) = &inner.active
        {
            status.levels = *active
                .levels
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        status
    }

    fn start(&self, mode: CaptureMode, call_app: Option<&str>) {
        {
            // One guard for the check and the change, so two starts at once
            // cannot both begin.
            let mut inner = self.inner();
            if inner.quitting || inner.status.state != RecordingState::Idle {
                return;
            }
            inner.status.state = RecordingState::Starting;
            inner.status.error = None;
            inner.status.warning = None;
        }
        self.notify();
        match self.start_inner(mode, call_app) {
            Ok(()) => self.warm_up_if_installed(),
            Err(error) => {
                let mut inner = self.inner();
                inner.status.state = RecordingState::Idle;
                inner.status.error = Some(format!("Recording could not start: {error}"));
            }
        }
        self.notify();
    }

    fn stop(&self) {
        let active = Self::begin_stop(&mut self.inner());
        if let Some(active) = active {
            self.finish_stop(active, RecordingEndReason::Manual);
        }
    }

    fn toggle(&self) {
        // Read first: a guard in the scrutinee would be held through the
        // arms, and `start` and `stop` lock again.
        let state = self.inner().status.state;
        match state {
            RecordingState::Idle => self.start(CaptureMode::Call, None),
            RecordingState::Recording => self.stop(),
            _ => {}
        }
    }

    fn keep_recording(&self) {
        self.inner().status.auto_stop = None;
        self.notify();
    }

    fn clear_messages(&self) {
        let mut inner = self.inner();
        inner.status.warning = None;
        inner.status.error = None;
        drop(inner);
        self.notify();
    }

    fn refresh_permissions(&self) {
        let denied = denied_permissions(steno_bridge::Platform::CURRENT, &*self.permissions);
        self.inner().status.denied_permissions = denied;
        self.notify();
    }
}

/// What a saved recording warns about, every line that applies joined
/// into one: a device that disappeared, frames that never reached the
/// files (in seconds of the lane that lost most, rounded to the nearest
/// second, so from half a second on: a lone 10 ms drift slip is not worth
/// a warning, and the log line keeps every count), and a call whose system
/// audio stayed silent. The dropped frames name no
/// cause, since the count holds several: the relay full behind a slow
/// disk, ring overruns while the computer was too busy, frames a stop left
/// undrained and, on Windows, the slips that absorb clock drift. Swift:
/// `RecordingController.stop`, where a device loss replaced the silent-lane
/// line and that line reads "The system audio lane stayed silent"; the
/// joining and the dropped frames are Rust only.
fn recording_warning(mode: CaptureMode, statistics: &CaptureStatistics) -> Option<String> {
    let mut lines = Vec::new();
    if statistics.ended_on_device_loss {
        lines.push("An audio device disappeared; the partial recording was kept.".to_owned());
    }
    let dropped = statistics
        .dropped_frames
        .values()
        .max()
        .copied()
        .unwrap_or(0);
    let seconds = (dropped + FRAMES_PER_SECOND / 2) / FRAMES_PER_SECOND;
    if seconds > 0 {
        lines.push(if seconds == 1 {
            "About 1 second of the recording is missing.".to_owned()
        } else {
            format!("About {seconds} seconds of the recording are missing.")
        });
    }
    if mode == CaptureMode::Call && statistics.system_lane_silent {
        lines.push(
            "Steno heard nothing from the call's audio. Check the system audio permission."
                .to_owned(),
        );
    }
    (!lines.is_empty()).then(|| lines.join(" "))
}

/// A `warn` line when a recording lost frames, with the meeting's id and
/// the count per lane (counts only, the log's privacy rule). Rust only.
fn log_dropped_frames(meeting_id: Uuid, statistics: &CaptureStatistics) {
    if statistics.dropped_frames.values().any(|frames| *frames > 0) {
        tracing::warn!(
            meeting = %meeting_id,
            dropped = ?statistics.dropped_frames,
            "the recording lost frames"
        );
    }
}

/// The required permissions `permissions` reports denied, among the ones
/// `platform` has: off the Mac, system audio has no switch of its own
/// (Windows' is the microphone's), so it is never reported denied there.
fn denied_permissions(
    platform: steno_bridge::Platform,
    permissions: &dyn Permissions,
) -> Vec<PermissionKind> {
    PermissionKind::for_platform(platform)
        .iter()
        .copied()
        .filter(|kind| steno_host::services::permission_is_required(*kind))
        .filter(|kind| permissions.state(*kind) == steno_bridge::PermissionState::Denied)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::app::BuildError;
    use crate::pipeline::{BuiltEngine, BuiltPipeline, MakeDependencies};
    use crate::testing::{
        PATIENCE, eventually, fake_dependencies, on_own_thread, synthetic_capture, temp_store,
    };
    use steno_audio::CaptureError;
    use steno_audio::writer::{LaneFrames, RecordingFiles, RecordingWriter, RecordingWriting};
    use steno_core::AudioLane;
    use steno_core::paths::file_url;
    use steno_core::testing::{FakeDiarizer, FakeSpeechEngine};
    use steno_host::fakes::{FakePermissions, FakeSpeechModels};
    use steno_speech::SpeechSettings;

    struct Harness {
        _dir: tempfile::TempDir,
        store: Arc<Store>,
        recorder: Arc<CaptureRecorder>,
        engine: Arc<FakeSpeechEngine>,
        diarizer: Arc<FakeDiarizer>,
        /// While set, a reload fails as a build error would and the
        /// current pipeline stays.
        failing_reloads: Arc<AtomicBool>,
    }

    /// Where the app runs each engine id with `speech_settings`.
    fn platform_rule(
        speech_settings: SpeechSettings,
    ) -> impl Fn(&str) -> SpeechRuntime + Send + Sync + 'static {
        move |engine_id| crate::speech::engine_runtime(engine_id, &speech_settings)
    }

    /// The Mac's default rule, on every platform: `parakeet-v3` in this
    /// process, every other id in the speech sidecar.
    fn mac_rule(engine_id: &str) -> SpeechRuntime {
        if engine_id == "parakeet-v3" {
            SpeechRuntime::CoreMlInProcess
        } else {
            SpeechRuntime::OnnxSidecar
        }
    }

    /// Fake models with `assets` on disk.
    fn models_with(assets: &[ModelAsset]) -> Arc<FakeSpeechModels> {
        let models = Arc::new(FakeSpeechModels::default());
        for asset in assets {
            models.set_installed(*asset, None);
        }
        models
    }

    /// A recorder over fake models with `installed` on disk.
    fn harness(installed: &[ModelAsset]) -> Harness {
        harness_over(
            models_with(installed),
            "parakeet-v3",
            platform_rule(SpeechSettings::default()),
        )
    }

    /// A recorder over `models`, the settings naming `engine_id`. Each
    /// build, the first and every reload, records the engine id stored
    /// at that moment and the runtime `runtime_of` gives it, as
    /// `app::pipeline_dependencies` does.
    fn harness_over(
        models: Arc<dyn SpeechModels>,
        engine_id: &str,
        runtime_of: impl Fn(&str) -> SpeechRuntime + Send + Sync + 'static,
    ) -> Harness {
        let (dir, store) = temp_store();
        let mut settings = store.settings().unwrap();
        settings.audio_folder = file_url(&dir.path().join("audio"), true);
        engine_id.clone_into(&mut settings.speech_engine_id);
        store.save_settings(&settings).unwrap();
        let engine = Arc::new(FakeSpeechEngine::default());
        let diarizer = Arc::new(FakeDiarizer::default());
        let mut dependencies = fake_dependencies(&store, "fake-engine")
            .with_speech_engine(steno_pipeline::SharedSpeechEngine::new(engine.clone()));
        dependencies.diarizer = diarizer.clone();
        let failing_reloads = Arc::new(AtomicBool::new(false));
        let make: MakeDependencies = {
            let (store, failing_reloads) = (store.clone(), failing_reloads.clone());
            Arc::new(move || {
                if failing_reloads.load(Ordering::SeqCst) {
                    return Err(BuildError::DatabaseFolder(std::io::Error::other(
                        "the build fails",
                    )));
                }
                let engine_id = store.settings()?.speech_engine_id;
                Ok(BuiltPipeline {
                    dependencies: dependencies.clone(),
                    engine: BuiltEngine {
                        runtime: runtime_of(&engine_id),
                        engine_id,
                    },
                })
            })
        };
        let pipeline = Arc::new(CurrentPipeline::new(
            make().unwrap(),
            make,
            tokio::runtime::Handle::current(),
        ));
        let recorder = Arc::new(CaptureRecorder::new(
            store.clone(),
            pipeline,
            synthetic_capture(),
            Arc::new(FakePermissions::all_granted()),
            models,
            chrono::FixedOffset::east_opt(0).unwrap(),
            tokio::runtime::Handle::current(),
        ));
        Harness {
            _dir: dir,
            store: store.clone(),
            recorder,
            engine,
            diarizer,
            failing_reloads,
        }
    }

    impl Harness {
        /// Stores `engine_id` as a Settings save does, then reloads.
        fn save_engine(&self, engine_id: &str) -> Result<(), BuildError> {
            let mut settings = self.store.settings().unwrap();
            engine_id.clone_into(&mut settings.speech_engine_id);
            self.store.save_settings(&settings).unwrap();
            self.recorder.pipeline.reload()
        }
    }

    async fn start(recorder: &Arc<CaptureRecorder>) {
        let starting = recorder.clone();
        tokio::task::spawn_blocking(move || starting.start(CaptureMode::InPerson, None))
            .await
            .unwrap();
        assert_eq!(recorder.status().state, RecordingState::Recording);
    }

    async fn stop(recorder: &Arc<CaptureRecorder>) {
        let stopping = recorder.clone();
        tokio::task::spawn_blocking(move || stopping.stop())
            .await
            .unwrap();
    }

    /// Quits on a thread of its own, failing the test rather than hanging
    /// it when quitting never returns.
    fn quit(recorder: &Arc<CaptureRecorder>) {
        let quitting = recorder.clone();
        on_own_thread(PATIENCE, "quitting returned", move || {
            quitting.stop_for_quit();
        });
    }

    /// Every change that finds the recorder in `state` holds it there for
    /// a while; the receiver hears of each one as it begins.
    fn held_in(
        recorder: &Arc<CaptureRecorder>,
        state: RecordingState,
    ) -> std::sync::mpsc::Receiver<()> {
        let (reached, seen) = std::sync::mpsc::channel();
        let watched = Arc::downgrade(recorder);
        recorder.on_change(Arc::new(move || {
            if watched
                .upgrade()
                .is_some_and(|recorder| recorder.status().state == state)
            {
                let _ = reached.send(());
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }));
        seen
    }

    /// Starts a recording and waits until its warm-up has loaded the
    /// diarizer, which `warm_up` loads after the speech engine; whether
    /// the speech engine was loaded too.
    async fn warmed_the_engine(harness: &Harness) -> bool {
        start(&harness.recorder).await;
        eventually("the diarizer was loaded while recording", || {
            harness.diarizer.preparations.count() > 0
        })
        .await;
        harness.engine.preparations.count() > 0
    }

    /// With the defaults, the Mac's `CoreML` engine and the diarizer are
    /// both loaded, as Swift did; off the Mac the speech sidecar runs
    /// Parakeet, so only the diarizer is.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_warms_the_pipeline_up_when_the_models_are_installed() {
        let harness = harness(&[ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer]);
        assert_eq!(warmed_the_engine(&harness).await, cfg!(target_os = "macos"));
        stop(&harness.recorder).await;
    }

    /// With Parakeet in the speech sidecar (here chosen on every
    /// platform), a recording's warm-up loads the diarizer only: the
    /// child would otherwise stay resident through the recording.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_with_speech_in_the_sidecar_warms_the_diarizer_only() {
        let harness = harness_over(
            models_with(&[ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer]),
            "parakeet-v3",
            platform_rule(crate::speech::testing::sidecar_chosen()),
        );
        assert!(!warmed_the_engine(&harness).await);
        stop(&harness.recorder).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_never_warms_up_models_that_would_download() {
        let harness = harness(&[ModelAsset::OfflineDiarizer]);
        start(&harness.recorder).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(harness.engine.preparations.count(), 0);
        assert_eq!(harness.diarizer.preparations.count(), 0);
        // Processing the recording loads them, as it always did.
        stop(&harness.recorder).await;
    }

    /// The warm-up follows the engine a reload built: the Mac's rule on
    /// every platform, so moving from an id the sidecar runs to the
    /// in-process `parakeet-v3` makes the recording load the engine.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_warm_up_follows_the_engine_a_reload_built() {
        let harness = harness_over(
            models_with(&[
                ModelAsset::ParakeetV3,
                ModelAsset::ParakeetUltra,
                ModelAsset::OfflineDiarizer,
            ]),
            "parakeet-ultra",
            mac_rule,
        );
        harness.save_engine("parakeet-v3").unwrap();
        assert!(warmed_the_engine(&harness).await);
        stop(&harness.recorder).await;
    }

    /// After a failed reload the pipeline keeps the engine it was built
    /// with, and so does the warm-up, whatever id the store holds now (a
    /// Swift app that saved another engine looks the same): a pipeline on
    /// the sidecar is not warmed into a child although the store names
    /// the in-process `parakeet-v3`, and a pipeline on `CoreML` still
    /// loads its engine although the store names a sidecar id. Each half
    /// installs only the built engine's model and the diarizer, so the
    /// installed check must ask about the built engine too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn after_a_failed_reload_the_warm_up_follows_the_engine_the_pipeline_kept() {
        let sidecar = harness_over(
            models_with(&[ModelAsset::ParakeetUltra, ModelAsset::OfflineDiarizer]),
            "parakeet-ultra",
            mac_rule,
        );
        sidecar.failing_reloads.store(true, Ordering::SeqCst);
        assert!(sidecar.save_engine("parakeet-v3").is_err());
        assert!(!warmed_the_engine(&sidecar).await);
        stop(&sidecar.recorder).await;

        let in_process = harness_over(
            models_with(&[ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer]),
            "parakeet-v3",
            mac_rule,
        );
        in_process.failing_reloads.store(true, Ordering::SeqCst);
        assert!(in_process.save_engine("parakeet-ultra").is_err());
        assert!(warmed_the_engine(&in_process).await);
        stop(&in_process.recorder).await;
    }

    /// Quitting stops the recording with `quit` and saves it: the meeting
    /// is queued for processing when the call returns.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quitting_stops_and_saves_the_recording_with_the_quit_reason() {
        let harness = harness(&[]);
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        quit(&harness.recorder);
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Quit));
        assert_ne!(
            meeting.state.kind(),
            steno_core::MeetingStateKind::Recording
        );
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
    }

    /// A Quit while a manual Stop is still saving waits for that save:
    /// when quitting returns the asset is written and the meeting keeps the
    /// manual stop's reason, rather than the app exiting mid-save.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quitting_during_a_stop_waits_for_its_save() {
        let harness = harness(&[]);
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let stop_seen = held_in(&harness.recorder, RecordingState::Stopping);
        let stopper = harness.recorder.clone();
        let manual = std::thread::spawn(move || stopper.stop());
        stop_seen.recv_timeout(PATIENCE).expect("the stop began");
        quit(&harness.recorder);
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Manual));
        manual.join().unwrap();
    }

    /// A Quit while a start is under way stops the recording it starts,
    /// rather than leaving it running into the exit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quitting_during_a_start_stops_the_recording_it_starts() {
        let harness = harness(&[]);
        let start_seen = held_in(&harness.recorder, RecordingState::Starting);
        let starter = harness.recorder.clone();
        let start = std::thread::spawn(move || starter.start(CaptureMode::InPerson, None));
        start_seen.recv_timeout(PATIENCE).expect("the start began");
        quit(&harness.recorder);
        start.join().unwrap();
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        let meetings = harness.store.all_meetings().unwrap();
        assert_eq!(meetings.len(), 1);
        assert_eq!(meetings[0].end_reason, Some(RecordingEndReason::Quit));
        assert!(harness.store.asset(meetings[0].id).unwrap().is_some());
    }

    /// Once quitting ran, a start (the tray's Record while the exit waits)
    /// records nothing, so no meeting is left recording past the exit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_recording_starts_after_quitting() {
        let harness = harness(&[]);
        quit(&harness.recorder);
        let starter = harness.recorder.clone();
        on_own_thread(PATIENCE, "the start returned", move || {
            starter.start(CaptureMode::InPerson, None);
        });
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        assert_eq!(harness.store.all_meetings().unwrap(), []);
    }

    /// Starts at the same moment begin one recording between them: the
    /// others return without a session, and stopping saves that one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_starts_begin_one_recording() {
        let harness = harness(&[]);
        let all_ready = Arc::new(std::sync::Barrier::new(8));
        let starters: Vec<_> = (0..8)
            .map(|_| {
                let (starter, ready) = (harness.recorder.clone(), all_ready.clone());
                std::thread::spawn(move || {
                    ready.wait();
                    starter.start(CaptureMode::InPerson, None);
                })
            })
            .collect();
        on_own_thread(PATIENCE, "every start returned", move || {
            for starter in starters {
                starter.join().unwrap();
            }
        });
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);
        assert_eq!(harness.store.all_meetings().unwrap().len(), 1);
        quit(&harness.recorder);
        let meetings = harness.store.all_meetings().unwrap();
        assert_eq!(meetings.len(), 1);
        assert!(harness.store.asset(meetings[0].id).unwrap().is_some());
    }

    /// The tray's Record and its Stop (`recording.toggle`): a toggle
    /// starts a call recording, the next one stops and saves it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_toggle_starts_a_call_and_the_next_one_stops_it() {
        let harness = harness(&[]);
        let toggling = harness.recorder.clone();
        on_own_thread(PATIENCE, "the first toggle returned", move || {
            toggling.toggle();
        });
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Recording);
        assert_eq!(status.mode, Some(CaptureMode::Call));
        let meeting_id = status.meeting_id.unwrap();
        let toggling = harness.recorder.clone();
        on_own_thread(PATIENCE, "the second toggle returned", move || {
            toggling.toggle();
        });
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
    }

    /// Quit while recording, as the shell's run loop drives it: the exit
    /// request is held, the recording is stopped once and saved, and only
    /// then the exit runs; by then the asset is written and the meeting is
    /// queued, and the pipeline goes on to process it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_quit_through_the_exit_gate_exits_after_the_recording_is_saved() {
        let harness = harness(&[]);
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let gate = crate::app::ExitGate::default();
        let (exited, exit_seen) = std::sync::mpsc::channel();
        let quitting = harness.recorder.clone();
        let (store, at_exit) = (harness.store.clone(), harness.recorder.clone());
        let held = gate.exit_requested(
            crate::app::SHUTDOWN_PATIENCE,
            move || quitting.stop_for_quit(),
            move || {
                let meeting = store.meeting(meeting_id).unwrap().unwrap();
                let saved = store.asset(meeting_id).unwrap().is_some();
                let _ = exited.send((at_exit.status().state, meeting, saved));
            },
        );
        assert!(!held, "the exit waits for the save");
        let (state, meeting, saved) = tokio::task::spawn_blocking(move || {
            exit_seen.recv_timeout(std::time::Duration::from_secs(20))
        })
        .await
        .unwrap()
        .expect("exited after the save");
        assert_eq!(state, RecordingState::Idle);
        assert!(saved, "the asset was written before the exit");
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Quit));
        assert_ne!(
            meeting.state.kind(),
            steno_core::MeetingStateKind::Recording
        );
        eventually("the pipeline picked the saved meeting up", || {
            harness.engine.transcriptions.count() > 0
        })
        .await;
    }

    /// The configured engine decides which model counts: with the `CoreML`
    /// Parakeet and the diarizer on disk but another engine id stored
    /// (Whisper, picked in Settings or in the Swift app), the speech
    /// sidecar would load the ONNX models, which are missing, and the
    /// warm-up neither loads nor downloads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_checks_the_models_of_the_configured_engine() {
        let models_dir = tempfile::tempdir().unwrap();
        let models = Arc::new(crate::speech::testing::models_in(models_dir.path()));
        crate::speech::testing::install_coreml_parakeet(&models.coreml);
        crate::speech::testing::install_onnx_diarizer(&models);
        assert!(models.is_installed(ModelAsset::OfflineDiarizer));
        let before = crate::speech::testing::files_under(models_dir.path());

        let harness = harness_over(
            models,
            "whisperkit-large-v3-turbo",
            platform_rule(SpeechSettings::default()),
        );
        start(&harness.recorder).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(harness.engine.preparations.count(), 0);
        assert_eq!(harness.diarizer.preparations.count(), 0);
        assert_eq!(
            crate::speech::testing::files_under(models_dir.path()),
            before,
            "nothing was downloaded"
        );
        stop(&harness.recorder).await;
    }

    #[test]
    fn only_the_platform_s_own_required_permissions_are_reported_denied() {
        use steno_bridge::{PermissionState, Platform};
        let denied = FakePermissions::all(PermissionState::Denied);
        assert_eq!(
            denied_permissions(Platform::Macos, &denied),
            [PermissionKind::Microphone, PermissionKind::SystemAudio],
        );
        assert_eq!(
            denied_permissions(Platform::Windows, &denied),
            [PermissionKind::Microphone]
        );
        assert_eq!(
            denied_permissions(Platform::Linux, &denied),
            [PermissionKind::Microphone]
        );
        let system_audio =
            FakePermissions::with_states([(PermissionKind::SystemAudio, PermissionState::Denied)]);
        assert_eq!(
            denied_permissions(Platform::Linux, &system_audio),
            Vec::new()
        );
        assert_eq!(
            denied_permissions(Platform::Macos, &system_audio),
            [PermissionKind::SystemAudio]
        );
    }

    fn statistics() -> CaptureStatistics {
        CaptureStatistics {
            duration: 60.0,
            dropped_frames: std::collections::BTreeMap::new(),
            system_lane_silent: false,
            ended_on_device_loss: false,
            device_changes: 0,
            gap_seconds: 0.0,
        }
    }

    #[test]
    fn a_clean_recording_warns_about_nothing() {
        assert_eq!(recording_warning(CaptureMode::Call, &statistics()), None);
    }

    /// Lost frames are a warning, in seconds of the lane that lost most,
    /// rounded to the nearest second: 2.5 s reads 3.
    #[test]
    fn dropped_frames_warn_with_the_seconds_missing() {
        let mut dropped = statistics();
        dropped.dropped_frames.insert(AudioLane::Mic, 250);
        dropped.dropped_frames.insert(AudioLane::System, 200);
        assert_eq!(
            recording_warning(CaptureMode::Call, &dropped).as_deref(),
            Some("About 3 seconds of the recording are missing.")
        );
        dropped.dropped_frames.clear();
        dropped.dropped_frames.insert(AudioLane::Mixed, 50);
        assert_eq!(
            recording_warning(CaptureMode::InPerson, &dropped).as_deref(),
            Some("About 1 second of the recording is missing.")
        );
    }

    /// Under half a second lost is no warning: one 10 ms slip, as Windows'
    /// drift correction makes, or 490 ms.
    #[test]
    fn under_half_a_second_lost_warns_about_nothing() {
        for frames in [1, 49] {
            let mut dropped = statistics();
            dropped.dropped_frames.insert(AudioLane::Mic, frames);
            assert_eq!(recording_warning(CaptureMode::InPerson, &dropped), None);
        }
    }

    /// Every warning that applies is kept: a device loss no longer hides a
    /// silent system lane or missing audio.
    #[test]
    fn every_warning_that_applies_is_joined() {
        let mut all = statistics();
        all.ended_on_device_loss = true;
        all.system_lane_silent = true;
        all.dropped_frames.insert(AudioLane::Mic, 100);
        let warning = recording_warning(CaptureMode::Call, &all).unwrap();
        assert_eq!(
            warning,
            "An audio device disappeared; the partial recording was kept. \
             About 1 second of the recording is missing. \
             Steno heard nothing from the call's audio. Check the system audio permission."
        );
        // In person there is no system lane to warn about.
        assert!(
            !recording_warning(CaptureMode::InPerson, &all)
                .unwrap()
                .contains("call's audio")
        );
    }

    /// A recording that lost frames leaves a `warn` line with the meeting
    /// and the counts per lane; a clean one leaves none.
    #[test]
    fn dropped_frames_are_logged_with_the_meeting_and_the_counts() {
        let log = steno_pipeline::fixtures::CapturedLog::warnings();
        let clean = Uuid::new_v4();
        log_dropped_frames(clean, &statistics());
        let lossy = Uuid::new_v4();
        let mut dropped = statistics();
        dropped.dropped_frames.insert(AudioLane::Mic, 250);
        dropped.dropped_frames.insert(AudioLane::System, 0);
        log_dropped_frames(lossy, &dropped);
        let text = log.text();
        assert!(!text.contains(&clean.to_string()), "{text}");
        let line = text
            .lines()
            .find(|line| line.contains(&lossy.to_string()))
            .unwrap_or_else(|| panic!("no line for the meeting: {text}"));
        assert!(line.contains("Mic: 250, System: 0"), "{line}");
    }

    /// The real writer whose first write waits for `go`, so a backend that
    /// delivers meanwhile overflows a one-frame relay.
    struct Stalled {
        inner: RecordingWriter,
        go: Option<std::sync::mpsc::Receiver<()>>,
    }

    impl RecordingWriting for Stalled {
        fn files(&self) -> RecordingFiles {
            self.inner.files()
        }
        fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
            if let Some(go) = self.go.take() {
                let _ = go.recv_timeout(PATIENCE);
            }
            self.inner.write(frames)
        }
        fn sync(&mut self) -> std::io::Result<()> {
            self.inner.sync()
        }
        fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
            self.inner.finish()
        }
    }

    /// A stop whose recording lost frames logs them with the meeting: a
    /// backend delivers two seconds at once while the writer is stalled
    /// behind a one-frame relay.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_that_lost_frames_logs_them() {
        use steno_audio::testing::SyntheticCaptureBackend;
        use steno_audio::testing::synthetic::SyntheticOptions;

        let log = steno_pipeline::fixtures::CapturedLog::warnings();
        let harness = harness(&[]);
        let (go, stalled) = std::sync::mpsc::channel();
        let stalled = Arc::new(Mutex::new(Some(stalled)));
        let backend = Arc::new(SyntheticCaptureBackend::new(SyntheticOptions::tones(
            &[AudioLane::Mixed],
            &[(AudioLane::Mixed, 440.0)],
            2.0,
        )));
        let make: MakeCaptureSession = {
            let backend = backend.clone();
            Arc::new(move |configuration| {
                let stalled = stalled.clone();
                CaptureSession::with_writer_factory(
                    configuration,
                    backend.clone(),
                    None,
                    1,
                    Arc::new(steno_audio::SystemClock::new()),
                    Arc::new(move |layout, lanes, keep_raw| {
                        Ok(Box::new(Stalled {
                            inner: RecordingWriter::new(layout, lanes, keep_raw)?,
                            go: stalled.lock().unwrap().take(),
                        }) as Box<dyn RecordingWriting>)
                    }),
                )
                .map_err(|error| error.to_string())
            })
        };
        let recorder = Arc::new(CaptureRecorder::new(
            harness.store.clone(),
            harness.recorder.pipeline.clone(),
            make,
            Arc::new(FakePermissions::all_granted()),
            harness.recorder.speech_models.clone(),
            FixedOffset::east_opt(0).unwrap(),
            tokio::runtime::Handle::current(),
        ));
        start(&recorder).await;
        let meeting_id = recorder.status().meeting_id.unwrap();
        let finished = backend.clone();
        tokio::task::spawn_blocking(move || finished.wait_until_finished())
            .await
            .unwrap();
        go.send(()).unwrap();
        stop(&recorder).await;
        let text = log.text();
        assert!(
            text.lines()
                .any(|line| line.contains(&meeting_id.to_string())
                    && line.contains("the recording lost frames")),
            "{text}"
        );
    }
}
