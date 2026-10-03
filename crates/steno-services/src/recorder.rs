//! The host's `Recorder` over the capture session and the Mac recording
//! intake. Swift: `apps/macos/Steno/Recording/RecordingController.swift`.
//! The calendar lookup, the auto-stop after a call ends and the detection
//! prompt wait for the shell's platform work (plan: `WP8`); the status
//! carries what the capture session reports.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use chrono::{FixedOffset, Utc};
use steno_audio::{CaptureConfiguration, CaptureSession, LaneLevels as AudioLevels};
use steno_bridge::{CaptureMode, PermissionKind, RecordingState};
use steno_core::{MeetingSource, RecordingEndReason, Store};
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
            }),
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
        if let Some(hook) = self.hook() {
            hook();
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn intake(&self) -> LocalRecordingIntake {
        LocalRecordingIntake::over(self.store.clone(), self.pipeline.current(), self.zone)
    }

    /// Loads the speech engine and the diarizer in the background while
    /// the recording runs, so processing does not wait for the cold load;
    /// only when both models are installed, so it never starts a
    /// download. Swift: `AppEnvironment.warmUpPipelineIfModelsInstalled`,
    /// called when a recording starts.
    fn warm_up_if_installed(&self) {
        if !(self.speech_models.is_installed(ModelAsset::ParakeetV3)
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

    fn start_inner(&self, mode: CaptureMode, call_app: Option<&str>) -> Result<(), String> {
        let settings = self.store.settings().map_err(|e| e.to_string())?;
        let audio_folder = steno_core::paths::path_from_file_url(&settings.audio_folder)
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
        Ok(())
    }

    fn stop_inner(&self, reason: RecordingEndReason) {
        let Some(active) = self.inner().active.take() else {
            return;
        };
        {
            let mut inner = self.inner();
            inner.status.state = RecordingState::Stopping;
        }
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
        if self.inner().status.state != RecordingState::Idle {
            return;
        }
        {
            let mut inner = self.inner();
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
        if self.inner().status.state != RecordingState::Recording {
            return;
        }
        self.stop_inner(RecordingEndReason::Manual);
        self.notify();
    }

    fn toggle(&self) {
        match self.inner().status.state {
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
    use crate::testing::{current_pipeline, eventually, fake_dependencies, temp_store};
    use steno_audio::testing::SyntheticCaptureBackend;
    use steno_audio::testing::synthetic::SyntheticOptions;
    use steno_core::paths::file_url;
    use steno_core::testing::{FakeDiarizer, FakeSpeechEngine};
    use steno_host::fakes::{FakePermissions, FakeSpeechModels};

    struct Harness {
        _dir: tempfile::TempDir,
        recorder: Arc<CaptureRecorder>,
        engine: Arc<FakeSpeechEngine>,
        diarizer: Arc<FakeDiarizer>,
    }

    fn harness(installed: &[ModelAsset]) -> Harness {
        let (dir, store) = temp_store();
        let mut settings = store.settings().unwrap();
        settings.audio_folder = file_url(&dir.path().join("audio"), true);
        store.save_settings(&settings).unwrap();
        let engine = Arc::new(FakeSpeechEngine::default());
        let diarizer = Arc::new(FakeDiarizer::default());
        let mut dependencies = fake_dependencies(&store, "fake-engine");
        dependencies.speech_engine = engine.clone();
        dependencies.diarizer = diarizer.clone();
        let pipeline = current_pipeline(dependencies);
        let models = Arc::new(FakeSpeechModels::default());
        for asset in installed {
            models.install(*asset, None);
        }
        let make_session: MakeCaptureSession = Arc::new(|configuration: CaptureConfiguration| {
            let lanes = configuration.lanes();
            let mut options =
                SyntheticOptions::tones(&lanes, &[(steno_core::AudioLane::Mic, 440.0)], 600.0);
            options.real_time = true;
            CaptureSession::with_backend(
                configuration,
                Arc::new(SyntheticCaptureBackend::new(options)),
                None,
                CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
                Arc::new(steno_audio::SystemClock::new()),
            )
            .map_err(|error| error.to_string())
        });
        let recorder = Arc::new(CaptureRecorder::new(
            store,
            pipeline,
            make_session,
            Arc::new(FakePermissions::all_granted()),
            models,
            chrono::FixedOffset::east_opt(0).unwrap(),
            tokio::runtime::Handle::current(),
        ));
        Harness {
            _dir: dir,
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
}
