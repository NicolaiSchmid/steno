//! The host's `Recorder` over the capture session and the Mac recording
//! intake. Swift: `apps/macos/Steno/Recording/RecordingController.swift`.
//! The calendar lookup, the auto-stop after a call ends and the detection
//! prompt are WP5's recorder policy (the plan's parity list); the status
//! carries what the capture session reports.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use chrono::{FixedOffset, Utc};
use steno_audio::{CaptureConfiguration, CaptureSession, LaneLevels as AudioLevels};
use steno_bridge::{CaptureMode, PermissionKind, RecordingState};
use steno_core::{MeetingSource, RecordingEndReason, Settings, Store};
use steno_host::services::{LaneLevels, Permissions, Recorder, RecorderStatus, SpeechModels};
use steno_host::speech::ModelAsset;
use steno_pipeline::{LocalRecordingIntake, RecordingResult};
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
    level_thread: Option<JoinHandle<()>>,
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

    /// Loads the speech engine and the diarizer in the background while
    /// the recording runs, so processing does not wait for the cold load;
    /// only when the models of the engine `settings` name and the
    /// diarizer's are installed, so it never starts a download. Swift:
    /// `AppEnvironment.warmUpPipelineIfModelsInstalled`, called when a
    /// recording starts.
    fn warm_up_if_installed(&self, settings: &Settings) {
        if !(self
            .speech_models
            .engine_installed(&settings.speech_engine_id)
            && self.speech_models.is_installed(ModelAsset::OfflineDiarizer))
        {
            return;
        }
        let pipeline = self.pipeline.current();
        self.runtime.spawn(async move {
            if let Err(failure) = pipeline.warm_up().await {
                tracing::debug!(%failure, "warm-up failed; processing loads the models");
            }
        });
    }

    /// Starts the session and the meeting; the settings it started under.
    fn start_inner(&self, mode: CaptureMode, call_app: Option<&str>) -> Result<Settings, String> {
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
                levels: shared.clone(),
                level_thread: None,
            });
        }
        // Levels arrive on a std channel at 10 Hz; a thread forwards them
        // into `Active::levels` and the host republishes `recording`.
        let hook = self.hook();
        let thread = std::thread::spawn(move || {
            while let Ok(update) = levels_receiver.recv() {
                *shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(levels(&update));
                if let Some(hook) = &hook {
                    hook();
                }
            }
        });
        if let Some(active) = self.inner().active.as_mut() {
            active.level_thread = Some(thread);
        }
        Ok(settings)
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
                    Ok(_) => {
                        let mut warning = None;
                        if active.mode == CaptureMode::Call && statistics.system_lane_silent {
                            warning = Some(
                                "The system audio lane stayed silent. Check the system audio permission."
                                    .to_owned(),
                            );
                        }
                        if statistics.ended_on_device_loss {
                            warning = Some(
                                "An audio device disappeared; the partial recording was kept."
                                    .to_owned(),
                            );
                        }
                        Ok(warning)
                    }
                    Err(error) => Err(format!("Recording could not be saved: {error}")),
                }
            }
            Err(error) => {
                let message = format!("Recording could not be saved: {error}");
                let _ = intake.fail(active.meeting_id, &message);
                Err(message)
            }
        };
        if let Some(thread) = active.level_thread {
            drop(active.session);
            let _ = thread.join();
        }
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
            Ok(settings) => self.warm_up_if_installed(&settings),
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
        let denied: Vec<PermissionKind> = PermissionKind::ALL
            .iter()
            .copied()
            .filter(|kind| steno_host::services::permission_is_required(*kind))
            .filter(|kind| self.permissions.state(*kind) == steno_bridge::PermissionState::Denied)
            .collect();
        self.inner().status.denied_permissions = denied;
        self.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        PATIENCE, current_pipeline, eventually, fake_dependencies, on_own_thread,
        synthetic_capture, temp_store,
    };
    use steno_core::paths::file_url;
    use steno_core::testing::{FakeDiarizer, FakeSpeechEngine};
    use steno_host::fakes::{FakePermissions, FakeSpeechModels};

    struct Harness {
        _dir: tempfile::TempDir,
        store: Arc<Store>,
        recorder: Arc<CaptureRecorder>,
        engine: Arc<FakeSpeechEngine>,
        diarizer: Arc<FakeDiarizer>,
    }

    /// A recorder over fake models with `installed` on disk.
    fn harness(installed: &[ModelAsset]) -> Harness {
        let models = Arc::new(FakeSpeechModels::default());
        for asset in installed {
            models.set_installed(*asset, None);
        }
        harness_over(models, "parakeet-v3")
    }

    /// A recorder over `models`, the settings naming `engine_id`.
    fn harness_over(models: Arc<dyn SpeechModels>, engine_id: &str) -> Harness {
        let (dir, store) = temp_store();
        let mut settings = store.settings().unwrap();
        settings.audio_folder = file_url(&dir.path().join("audio"), true);
        engine_id.clone_into(&mut settings.speech_engine_id);
        store.save_settings(&settings).unwrap();
        let engine = Arc::new(FakeSpeechEngine::default());
        let diarizer = Arc::new(FakeDiarizer::default());
        let mut dependencies = fake_dependencies(&store, "fake-engine");
        dependencies.speech_engine = engine.clone();
        dependencies.diarizer = diarizer.clone();
        let pipeline = current_pipeline(dependencies);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_warms_the_pipeline_up_when_the_models_are_installed() {
        let harness = harness(&[ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer]);
        start(&harness.recorder).await;
        eventually("both models were loaded while recording", || {
            harness.engine.preparations.count() > 0 && harness.diarizer.preparations.count() > 0
        })
        .await;
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
    async fn starts_at_once_begin_one_recording() {
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
    /// Parakeet and the diarizer on disk but another engine id stored (a
    /// Swift user who picked Whisper), the ONNX engine would load, its
    /// models are missing, and the warm-up neither loads nor downloads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_checks_the_models_of_the_configured_engine() {
        let models_dir = tempfile::tempdir().unwrap();
        let models = Arc::new(crate::speech::ModelStoreSpeechModels::new(
            models_dir.path(),
        ));
        crate::speech::testing::install_coreml_parakeet(&models.coreml);
        crate::speech::testing::install_onnx_diarizer(&models);
        assert!(models.is_installed(ModelAsset::OfflineDiarizer));
        let before = crate::speech::testing::files_under(models_dir.path());

        let harness = harness_over(models, "whisperkit-large-v3-turbo");
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
}
