//! Fakes for every service in [`services`](crate::services), after
//! `Services/Fakes.swift` and the modules' `Testing/` fakes: each holds its
//! state behind a mutex, answers what a test set, and records what the
//! host asked, so the host runs without a shell on a real store. The product never
//! constructs these; [`Services`] built from them is what the tests and the
//! CLI's dry run use.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use steno_bridge::{BridgeWindow, CaptureMode, PermissionKind, PermissionState, RecordingState};
use steno_core::protocols::BoundaryResult;
use steno_core::testing::InMemorySecretStore;
use steno_core::{AudioRetention, HandoverReceipt, ObsidianSettings, PairedDevice, Settings};
use uuid::Uuid;

use crate::services::{
    AudioDevices, AutoStopStatus, ClipPlayer, Clock, CodexModel, CodexModelsError, ExportValidator,
    FileSystem, FolderUsage, Handover, INSTALLING_UPDATE, InputDevice, LeftRecording,
    ListenerState, LlmService, LoginItem, LoginItemStatus, Opener, PairingCode, Permissions,
    Pipeline, Preferences, ProcessAgainRefusal, QrEncoder, Recorder, RecorderStatus, Services,
    SpeechModels, StartHold, UpdateOutcome, Updater, permission_is_required,
};
use crate::speech::ModelAsset;

/// A poisoned fake is still readable: a panic in one test thread must not
/// hide the state the assertion wants to see.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The hook a test installed, cloned out so the fake's lock is released
/// before the hook runs: a hook may call its fake again.
fn hook_of<H: ?Sized>(slot: &Mutex<Option<Arc<H>>>) -> Option<Arc<H>> {
    lock(slot).clone()
}

/// A fixed `now`. Swift: the `now: { TestSupport.now }` closure.
#[derive(Debug)]
pub struct FakeClock {
    now: Mutex<DateTime<Utc>>,
}

impl FakeClock {
    #[must_use]
    pub fn new(now: DateTime<Utc>) -> Self {
        FakeClock {
            now: Mutex::new(now),
        }
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *lock(&self.now) = now;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *lock(&self.now)
    }
}

/// Swift: `FakeLoginItem`.
#[derive(Debug)]
pub struct FakeLoginItem {
    status: Mutex<LoginItemStatus>,
    /// Every `set_enabled` call, in order.
    pub changes: Mutex<Vec<bool>>,
    pub opened: Mutex<usize>,
    /// Set to make `set_enabled` fail with this text.
    pub failure: Mutex<Option<String>>,
}

impl FakeLoginItem {
    #[must_use]
    pub fn new(status: LoginItemStatus) -> Self {
        FakeLoginItem {
            status: Mutex::new(status),
            changes: Mutex::new(Vec::new()),
            opened: Mutex::new(0),
            failure: Mutex::new(None),
        }
    }

    /// What `status` answers from now on, as the system would report it.
    pub fn set_status(&self, status: LoginItemStatus) {
        *lock(&self.status) = status;
    }

    /// Makes every `set_enabled` fail with `text`; `None` lets it through.
    pub fn fail_changes(&self, text: Option<&str>) {
        *lock(&self.failure) = text.map(str::to_owned);
    }
}

impl LoginItem for FakeLoginItem {
    fn status(&self) -> LoginItemStatus {
        *lock(&self.status)
    }

    fn set_enabled(&self, enabled: bool) -> BoundaryResult<()> {
        if let Some(failure) = lock(&self.failure).clone() {
            return Err(failure.into());
        }
        lock(&self.changes).push(enabled);
        *lock(&self.status) = if enabled {
            LoginItemStatus::Enabled
        } else {
            LoginItemStatus::NotRegistered
        };
        Ok(())
    }

    fn open_system_settings(&self) {
        *lock(&self.opened) += 1;
    }
}

/// A hook a test installs to observe the host mid-request: the Swift view
/// models published `isRequesting` while the system prompt was up, and a
/// blocking host shows that state only to whoever the fake calls back.
/// The fake calls it with none of its own locks held, so the hook may call
/// the fake again.
pub type RequestHook = Arc<dyn Fn(PermissionKind) + Send + Sync>;

/// Swift: `FakePermissions`.
pub struct FakePermissions {
    pub states: Mutex<BTreeMap<PermissionKind, PermissionState>>,
    /// What `request` answers per kind; `granted` when unset.
    pub answers: Mutex<BTreeMap<PermissionKind, PermissionState>>,
    pub requests: Mutex<Vec<PermissionKind>>,
    pub opened_panes: Mutex<Vec<PermissionKind>>,
    pub on_request: Mutex<Option<RequestHook>>,
}

impl std::fmt::Debug for FakePermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakePermissions")
            .field("states", &*lock(&self.states))
            .finish_non_exhaustive()
    }
}

impl FakePermissions {
    /// Every kind in `state`.
    #[must_use]
    pub fn all(state: PermissionState) -> Self {
        Self::with_states(PermissionKind::ALL.iter().map(|kind| (*kind, state)))
    }

    #[must_use]
    pub fn all_granted() -> Self {
        Self::all(PermissionState::Granted)
    }

    #[must_use]
    pub fn with_states(
        states: impl IntoIterator<Item = (PermissionKind, PermissionState)>,
    ) -> Self {
        FakePermissions {
            states: Mutex::new(states.into_iter().collect()),
            answers: Mutex::new(BTreeMap::new()),
            requests: Mutex::new(Vec::new()),
            opened_panes: Mutex::new(Vec::new()),
            on_request: Mutex::new(None),
        }
    }

    /// What `state` reports for `kind`.
    pub fn set_state(&self, kind: PermissionKind, state: PermissionState) {
        lock(&self.states).insert(kind, state);
    }

    /// What `request` answers for `kind`.
    pub fn set_answer(&self, kind: PermissionKind, state: PermissionState) {
        lock(&self.answers).insert(kind, state);
    }

    /// Runs `hook` inside every `request`, before it answers.
    pub fn set_on_request(&self, hook: impl Fn(PermissionKind) + Send + Sync + 'static) {
        *lock(&self.on_request) = Some(Arc::new(hook));
    }
}

impl Permissions for FakePermissions {
    fn state(&self, kind: PermissionKind) -> PermissionState {
        lock(&self.states)
            .get(&kind)
            .copied()
            .unwrap_or(PermissionState::Unknown)
    }

    fn request(&self, kind: PermissionKind) -> PermissionState {
        lock(&self.requests).push(kind);
        if let Some(hook) = hook_of(&self.on_request) {
            hook(kind);
        }
        let answer = lock(&self.answers)
            .get(&kind)
            .copied()
            .unwrap_or(PermissionState::Granted);
        lock(&self.states).insert(kind, answer);
        answer
    }

    fn open_system_settings(&self, kind: PermissionKind) {
        lock(&self.opened_panes).push(kind);
    }
}

/// Swift: `FakeUpdater`.
#[derive(Debug)]
pub struct FakeUpdater {
    pub can_check: Mutex<bool>,
    pub checks_automatically: Mutex<bool>,
    pub downloads_automatically: Mutex<bool>,
    pub last_check: Mutex<Option<DateTime<Utc>>>,
    pub outcome: Mutex<UpdateOutcome>,
    pub checks: Mutex<usize>,
}

impl Default for FakeUpdater {
    fn default() -> Self {
        FakeUpdater {
            can_check: Mutex::new(true),
            checks_automatically: Mutex::new(true),
            downloads_automatically: Mutex::new(false),
            last_check: Mutex::new(None),
            outcome: Mutex::new(UpdateOutcome::NotChecked),
            checks: Mutex::new(0),
        }
    }
}

impl FakeUpdater {
    /// When the last check ran and what it found.
    pub fn set_last_check(&self, at: Option<DateTime<Utc>>, outcome: UpdateOutcome) {
        *lock(&self.last_check) = at;
        *lock(&self.outcome) = outcome;
    }
}

impl Updater for FakeUpdater {
    fn can_check_for_updates(&self) -> bool {
        *lock(&self.can_check)
    }

    fn automatically_checks(&self) -> bool {
        *lock(&self.checks_automatically)
    }

    fn set_automatically_checks(&self, enabled: bool) {
        *lock(&self.checks_automatically) = enabled;
    }

    fn automatically_downloads(&self) -> bool {
        *lock(&self.downloads_automatically)
    }

    fn set_automatically_downloads(&self, enabled: bool) {
        *lock(&self.downloads_automatically) = enabled;
    }

    fn last_check_at(&self) -> Option<DateTime<Utc>> {
        *lock(&self.last_check)
    }

    fn last_outcome(&self) -> UpdateOutcome {
        lock(&self.outcome).clone()
    }

    fn check_for_updates(&self) {
        *lock(&self.checks) += 1;
    }
}

/// A recorder that runs the state machine without a capture session: start
/// goes straight to `recording` with a fresh meeting id, stop back to idle;
/// levels, the auto-stop and the messages are whatever the test sets.
/// A start under a [`StartHold`] is refused, as the capture recorder's is.
/// Swift: the `RecordingController` over the synthetic backend, reduced to
/// what the window reads.
pub struct FakeRecorder {
    status: Arc<Mutex<RecorderStatus>>,
    /// The start holds alive.
    holds: Arc<Mutex<usize>>,
    clock: Arc<dyn Clock>,
    permissions: Arc<dyn Permissions>,
    pub starts: Mutex<Vec<(CaptureMode, Option<String>)>>,
    pub stops: Mutex<usize>,
    pub kept: Mutex<usize>,
    /// Every folder `remember_audio_folder` was given, in order.
    pub remembered: Mutex<Vec<PathBuf>>,
    /// What `left_recording` answers per meeting; the default otherwise.
    pub left: Mutex<BTreeMap<Uuid, LeftRecording>>,
    /// Every meeting `left_recording` was asked about, in order.
    pub asked: Mutex<Vec<Uuid>>,
    /// Every meeting `forget_recording` forgot, in order.
    pub forgotten: Mutex<Vec<Uuid>>,
    /// The recorder's entries: `forget_recording` takes one out and
    /// returns it, `restore_recording` puts it back.
    pub recorded: Mutex<BTreeMap<Uuid, PathBuf>>,
    /// `forget_recording` fails, as the support folder can when it is
    /// read-only or full.
    pub forget_fails: Mutex<bool>,
    /// Runs once inside the next `restore_recording`, before it records
    /// the entry: a test's moment between the guard's read of the row and
    /// its restore.
    pub before_restore: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl FakeRecorder {
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, permissions: Arc<dyn Permissions>) -> Self {
        FakeRecorder {
            status: Arc::new(Mutex::new(RecorderStatus::idle())),
            holds: Arc::default(),
            clock,
            permissions,
            starts: Mutex::new(Vec::new()),
            stops: Mutex::new(0),
            kept: Mutex::new(0),
            remembered: Mutex::new(Vec::new()),
            left: Mutex::new(BTreeMap::new()),
            forgotten: Mutex::new(Vec::new()),
            recorded: Mutex::new(BTreeMap::new()),
            forget_fails: Mutex::new(false),
            asked: Mutex::new(Vec::new()),
            before_restore: Mutex::new(None),
        }
    }

    /// Overwrites the status wholesale (a test staging a live recording).
    pub fn set_status(&self, status: RecorderStatus) {
        *lock(&self.status) = status;
    }

    pub fn set_auto_stop(&self, auto_stop: Option<AutoStopStatus>) {
        lock(&self.status).auto_stop = auto_stop;
    }

    pub fn set_messages(&self, warning: Option<&str>, error: Option<&str>) {
        let mut status = lock(&self.status);
        status.warning = warning.map(str::to_owned);
        status.error = error.map(str::to_owned);
    }
}

impl Recorder for FakeRecorder {
    fn status(&self) -> RecorderStatus {
        lock(&self.status).clone()
    }

    fn start(&self, mode: CaptureMode, call_app: Option<&str>) {
        lock(&self.starts).push((mode, call_app.map(str::to_owned)));
        let mut status = lock(&self.status);
        if status.state != RecordingState::Idle {
            return;
        }
        if *lock(&self.holds) > 0 {
            status.error = Some(INSTALLING_UPDATE.to_owned());
            return;
        }
        status.state = RecordingState::Recording;
        status.started_at = Some(self.clock.now());
        status.mode = Some(mode);
        status.call_app = call_app.map(str::to_owned);
        status.meeting_id = Some(Uuid::new_v4());
        status.warning = None;
        status.error = None;
    }

    fn hold_starts(&self) -> StartHold {
        // The status's lock first, as `start` takes it.
        let _status = lock(&self.status);
        *lock(&self.holds) += 1;
        Box::new(FakeStartHold {
            status: self.status.clone(),
            holds: self.holds.clone(),
        })
    }

    fn stop(&self) {
        *lock(&self.stops) += 1;
        let mut status = lock(&self.status);
        if status.state != RecordingState::Recording {
            return;
        }
        *status = RecorderStatus {
            denied_permissions: status.denied_permissions.clone(),
            ..RecorderStatus::idle()
        };
    }

    fn toggle(&self) {
        let state = lock(&self.status).state;
        match state {
            RecordingState::Idle => self.start(CaptureMode::Call, None),
            RecordingState::Recording => self.stop(),
            RecordingState::Starting | RecordingState::Stopping => {}
        }
    }

    fn keep_recording(&self) {
        let mut status = lock(&self.status);
        if status.auto_stop.is_some() {
            *lock(&self.kept) += 1;
            status.auto_stop = None;
        }
    }

    fn clear_messages(&self) {
        let mut status = lock(&self.status);
        status.warning = None;
        status.error = None;
    }

    fn refresh_permissions(&self) {
        let denied: Vec<PermissionKind> = PermissionKind::ALL
            .iter()
            .copied()
            .filter(|kind| permission_is_required(*kind))
            .filter(|kind| self.permissions.state(*kind) == PermissionState::Denied)
            .collect();
        lock(&self.status).denied_permissions = denied;
    }

    fn remember_audio_folder(&self, folder: &Path) {
        lock(&self.remembered).push(folder.to_path_buf());
    }

    fn left_recording(&self, meeting_id: Uuid) -> LeftRecording {
        lock(&self.asked).push(meeting_id);
        lock(&self.left)
            .get(&meeting_id)
            .cloned()
            .unwrap_or_default()
    }

    fn forget_recording(&self, meeting_id: Uuid) -> std::io::Result<Option<PathBuf>> {
        if *lock(&self.forget_fails) {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        }
        lock(&self.forgotten).push(meeting_id);
        Ok(lock(&self.recorded).remove(&meeting_id))
    }

    fn restore_recording(&self, meeting_id: Uuid, folder: &Path) {
        let hook = lock(&self.before_restore).take();
        if let Some(hook) = hook {
            hook();
        }
        lock(&self.recorded).insert(meeting_id, folder.to_path_buf());
    }
}

/// [`FakeRecorder::hold_starts`]'s hold: the last one dropped clears the
/// refusal's error.
struct FakeStartHold {
    status: Arc<Mutex<RecorderStatus>>,
    holds: Arc<Mutex<usize>>,
}

impl Drop for FakeStartHold {
    fn drop(&mut self) {
        let mut status = lock(&self.status);
        let mut holds = lock(&self.holds);
        *holds -= 1;
        if *holds == 0 {
            status.error.take_if(|error| *error == INSTALLING_UPDATE);
        }
    }
}

/// Records every call; each operation fails with the text a test set.
/// Swift: the preview pipeline's fakes, reduced to the calls the view models
/// make.
#[derive(Debug, Default)]
pub struct FakePipeline {
    pub summary_reruns: Mutex<Vec<(Uuid, String)>>,
    pub redeliveries: Mutex<Vec<Uuid>>,
    /// Every meeting `process_again` was asked for, refused or not.
    pub processed_again: Mutex<Vec<Uuid>>,
    /// The refusal `process_again` answers with; `None` lets it through
    /// unless [`fail_calls`](Self::fail_calls) set a failure.
    pub process_again_refusal: Mutex<Option<ProcessAgainRefusal>>,
    pub retention: Mutex<Vec<(Uuid, AudioRetention)>>,
    pub reloads: Mutex<usize>,
    pub kept_forever: Mutex<i64>,
    pub failure: Mutex<Option<String>>,
    /// The meetings `export_keeps_failing` answers yes for.
    pub keeps_failing: Mutex<Vec<Uuid>>,
    /// What `damaged_audio_parts` answers per meeting; 0 for the others.
    pub damaged_audio: Mutex<std::collections::BTreeMap<Uuid, u32>>,
}

impl FakePipeline {
    /// Makes every call fail with `text`; `None` lets them through.
    pub fn fail_calls(&self, text: Option<&str>) {
        *lock(&self.failure) = text.map(str::to_owned);
    }

    /// How many recordings `keep_all_recordings` reports it kept.
    pub fn set_kept_forever(&self, count: i64) {
        *lock(&self.kept_forever) = count;
    }

    fn outcome(&self) -> BoundaryResult<()> {
        lock(&self.failure)
            .clone()
            .map_or(Ok(()), |text| Err(text.into()))
    }
}

impl Pipeline for FakePipeline {
    fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> BoundaryResult<()> {
        lock(&self.summary_reruns).push((meeting_id, template_id.to_owned()));
        self.outcome()
    }

    fn redeliver(&self, meeting_id: Uuid) -> BoundaryResult<()> {
        lock(&self.redeliveries).push(meeting_id);
        self.outcome()
    }

    fn export_keeps_failing(&self, meeting_id: Uuid) -> bool {
        lock(&self.keeps_failing).contains(&meeting_id)
    }

    fn process_again(&self, meeting_id: Uuid) -> Result<(), ProcessAgainRefusal> {
        lock(&self.processed_again).push(meeting_id);
        if let Some(refusal) = lock(&self.process_again_refusal).clone() {
            return Err(refusal);
        }
        self.outcome()
            .map_err(|error| ProcessAgainRefusal::CouldNotStart(error.to_string()))
    }

    fn damaged_audio_parts(&self, meeting_id: Uuid) -> u32 {
        lock(&self.damaged_audio)
            .get(&meeting_id)
            .copied()
            .unwrap_or(0)
    }

    fn apply_retention(&self, meeting_id: Uuid, rule: AudioRetention) -> BoundaryResult<()> {
        lock(&self.retention).push((meeting_id, rule));
        self.outcome()
    }

    fn reload(&self) -> BoundaryResult<()> {
        *lock(&self.reloads) += 1;
        self.outcome()
    }

    fn keep_all_recordings(&self) -> BoundaryResult<i64> {
        self.outcome()?;
        Ok(*lock(&self.kept_forever))
    }
}

/// A hook a test installs to observe the host while a download runs on
/// its thread: called once per download, before the first progress report,
/// so a test can hold the download and look at what the host published.
/// Called with none of the fake's locks held.
pub type DownloadHook = Arc<dyn Fn(ModelAsset) + Send + Sync>;

/// Installed assets with their sizes; a download reports its `progress`
/// steps and installs. Swift: `ModelStore` over `FakeModelDownloader`.
pub struct FakeSpeechModels {
    pub installed: Mutex<BTreeMap<ModelAsset, Option<i64>>>,
    pub downloads: Mutex<Vec<ModelAsset>>,
    pub removals: Mutex<Vec<ModelAsset>>,
    /// The `(fraction, phase)` reports of one download, in order.
    pub progress: Mutex<Vec<(f64, String)>>,
    pub download_failure: Mutex<Option<String>>,
    pub on_download: Mutex<Option<DownloadHook>>,
}

impl std::fmt::Debug for FakeSpeechModels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeSpeechModels")
            .field("installed", &*lock(&self.installed))
            .finish_non_exhaustive()
    }
}

impl Default for FakeSpeechModels {
    fn default() -> Self {
        FakeSpeechModels {
            installed: Mutex::new(BTreeMap::new()),
            downloads: Mutex::new(Vec::new()),
            removals: Mutex::new(Vec::new()),
            progress: Mutex::new(vec![
                (0.5, "downloading".to_owned()),
                (1.0, "installing".to_owned()),
            ]),
            download_failure: Mutex::new(None),
            on_download: Mutex::new(None),
        }
    }
}

impl FakeSpeechModels {
    /// Makes `asset` installed with `bytes` on disk.
    pub fn set_installed(&self, asset: ModelAsset, bytes: Option<i64>) {
        lock(&self.installed).insert(asset, bytes);
    }

    /// The `(fraction, phase)` reports every download makes, in order.
    pub fn set_progress(&self, steps: Vec<(f64, &str)>) {
        *lock(&self.progress) = steps
            .into_iter()
            .map(|(fraction, phase)| (fraction, phase.to_owned()))
            .collect();
    }

    /// Makes every download fail with `text` after its progress reports.
    pub fn fail_downloads(&self, text: Option<&str>) {
        *lock(&self.download_failure) = text.map(str::to_owned);
    }

    /// Runs `hook` at the start of every download.
    pub fn set_on_download(&self, hook: impl Fn(ModelAsset) + Send + Sync + 'static) {
        *lock(&self.on_download) = Some(Arc::new(hook));
    }
}

impl SpeechModels for FakeSpeechModels {
    fn is_installed(&self, asset: ModelAsset) -> bool {
        lock(&self.installed).contains_key(&asset)
    }

    fn installed_size(&self, asset: ModelAsset) -> Option<i64> {
        lock(&self.installed).get(&asset).copied().flatten()
    }

    fn download(
        &self,
        asset: ModelAsset,
        progress: &mut dyn FnMut(f64, &str),
    ) -> BoundaryResult<()> {
        lock(&self.downloads).push(asset);
        if let Some(hook) = hook_of(&self.on_download) {
            hook(asset);
        }
        let steps = lock(&self.progress).clone();
        for (fraction, phase) in &steps {
            progress(*fraction, phase);
        }
        if let Some(failure) = lock(&self.download_failure).clone() {
            return Err(failure.into());
        }
        lock(&self.installed).insert(asset, Some(asset.approximate_bytes()));
        Ok(())
    }

    fn remove(&self, asset: ModelAsset) -> BoundaryResult<()> {
        lock(&self.removals).push(asset);
        lock(&self.installed).remove(&asset);
        Ok(())
    }
}

/// A hook a test installs to observe the host while a probe runs: the
/// Swift view model published `isTesting` while the request was out, and
/// a blocking host shows that state only to whoever the fake calls back.
/// Called with none of the fake's locks held.
pub type ProbeHook = Arc<dyn Fn(&Settings) + Send + Sync>;

/// Answers the probe and the Codex sign-in with what a test set.
pub struct FakeLlmService {
    pub probe_result: Mutex<Result<String, String>>,
    pub codex_account: Mutex<Result<String, String>>,
    pub codex_models: Mutex<Result<Vec<CodexModel>, CodexModelsError>>,
    pub probes: Mutex<Vec<Settings>>,
    pub on_probe: Mutex<Option<ProbeHook>>,
    pub on_codex_models: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl std::fmt::Debug for FakeLlmService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeLlmService")
            .field("probes", &lock(&self.probes).len())
            .finish_non_exhaustive()
    }
}

impl Default for FakeLlmService {
    fn default() -> Self {
        FakeLlmService {
            probe_result: Mutex::new(Ok(
                "Connected: model listed, structured output jsonSchema, 12 ms.".to_owned(),
            )),
            codex_account: Mutex::new(Err("no Codex sign-in on this computer".to_owned())),
            codex_models: Mutex::new(Ok(Vec::new())),
            probes: Mutex::new(Vec::new()),
            on_probe: Mutex::new(None),
            on_codex_models: Mutex::new(None),
        }
    }
}

impl FakeLlmService {
    /// The Test button's line, or the failure text.
    pub fn set_probe_result(&self, result: Result<&str, &str>) {
        *lock(&self.probe_result) = result.map(str::to_owned).map_err(str::to_owned);
    }

    /// The Codex sign-in's account line, or why there is none.
    pub fn set_codex_account(&self, account: Result<&str, &str>) {
        *lock(&self.codex_account) = account.map(str::to_owned).map_err(str::to_owned);
    }

    /// The Codex models on offer, or why the list failed.
    pub fn set_codex_models(&self, models: Result<Vec<CodexModel>, CodexModelsError>) {
        *lock(&self.codex_models) = models;
    }

    /// Runs `hook` inside every probe, before it answers.
    pub fn set_on_probe(&self, hook: impl Fn(&Settings) + Send + Sync + 'static) {
        *lock(&self.on_probe) = Some(Arc::new(hook));
    }

    /// Runs `hook` inside every model list fetch, before it answers; `None`
    /// removes it.
    pub fn set_on_codex_models(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *lock(&self.on_codex_models) = hook;
    }
}

impl LlmService for FakeLlmService {
    fn probe(&self, settings: &Settings, _api_key: Option<&str>) -> BoundaryResult<String> {
        lock(&self.probes).push(settings.clone());
        if let Some(hook) = hook_of(&self.on_probe) {
            hook(settings);
        }
        lock(&self.probe_result).clone().map_err(Into::into)
    }

    fn codex_account(&self) -> BoundaryResult<String> {
        lock(&self.codex_account).clone().map_err(Into::into)
    }

    fn codex_models(&self) -> Result<Vec<CodexModel>, CodexModelsError> {
        if let Some(hook) = hook_of(&self.on_codex_models) {
            hook();
        }
        lock(&self.codex_models).clone()
    }
}

/// Accepts every vault unless a failure text is set.
#[derive(Debug, Default)]
pub struct FakeExportValidator {
    pub failure: Mutex<Option<String>>,
    pub validated: Mutex<Vec<ObsidianSettings>>,
}

impl FakeExportValidator {
    /// Makes every validation fail with `text`; `None` accepts again.
    pub fn fail_validation(&self, text: Option<&str>) {
        *lock(&self.failure) = text.map(str::to_owned);
    }
}

impl ExportValidator for FakeExportValidator {
    fn validate(&self, settings: &ObsidianSettings) -> BoundaryResult<()> {
        lock(&self.validated).push(settings.clone());
        lock(&self.failure)
            .clone()
            .map_or(Ok(()), |text| Err(text.into()))
    }
}

/// A listener with paired devices and receipts a test sets; `start` begins
/// listening on `port`, pairing opens a code that `pair` closes.
pub struct FakeHandover {
    pub state: Mutex<ListenerState>,
    pub mac_id: String,
    pub port: u16,
    pub devices: Mutex<Vec<PairedDevice>>,
    pub receipts: Mutex<Vec<HandoverReceipt>>,
    pub pairing: Mutex<Option<PairingCode>>,
    pub revoked: Mutex<Vec<Uuid>>,
    pub starts: Mutex<usize>,
    pub stops: Mutex<usize>,
    /// Set to make `start` fail with this text.
    pub start_failure: Mutex<Option<String>>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for FakeHandover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeHandover")
            .field("mac_id", &self.mac_id)
            .field("state", &*lock(&self.state))
            .finish_non_exhaustive()
    }
}

impl FakeHandover {
    #[must_use]
    pub fn new(mac_id: &str, port: u16, clock: Arc<dyn Clock>) -> Self {
        FakeHandover {
            state: Mutex::new(ListenerState::Stopped),
            mac_id: mac_id.to_owned(),
            port,
            devices: Mutex::new(Vec::new()),
            receipts: Mutex::new(Vec::new()),
            pairing: Mutex::new(None),
            revoked: Mutex::new(Vec::new()),
            starts: Mutex::new(0),
            stops: Mutex::new(0),
            start_failure: Mutex::new(None),
            clock,
        }
    }

    /// The phone arrived: a device joins the list.
    pub fn pair(&self, device: PairedDevice) {
        lock(&self.devices).push(device);
    }
}

impl Handover for FakeHandover {
    fn state(&self) -> ListenerState {
        lock(&self.state).clone()
    }

    fn mac_id(&self) -> String {
        self.mac_id.clone()
    }

    fn paired_devices(&self) -> BoundaryResult<Vec<PairedDevice>> {
        Ok(lock(&self.devices).clone())
    }

    fn start(&self) -> BoundaryResult<()> {
        *lock(&self.starts) += 1;
        if let Some(failure) = lock(&self.start_failure).clone() {
            *lock(&self.state) = ListenerState::Failed(failure.clone());
            return Err(failure.into());
        }
        *lock(&self.state) = ListenerState::Listening(self.port);
        Ok(())
    }

    fn stop(&self) {
        *lock(&self.stops) += 1;
        *lock(&self.state) = ListenerState::Stopped;
    }

    fn begin_pairing(&self) -> PairingCode {
        let code = PairingCode {
            expires_at: self.clock.now() + chrono::Duration::seconds(240),
            url_string: format!("steno://pair/v1?mac={}&name=Steno", self.mac_id),
        };
        *lock(&self.pairing) = Some(code.clone());
        code
    }

    fn cancel_pairing(&self) {
        *lock(&self.pairing) = None;
    }

    fn revoke(&self, device_id: Uuid) -> BoundaryResult<()> {
        lock(&self.revoked).push(device_id);
        lock(&self.devices).retain(|device| device.id != device_id);
        Ok(())
    }

    fn receipts(&self) -> Vec<HandoverReceipt> {
        lock(&self.receipts).clone()
    }
}

/// Returns the PNG a test set for every text. Swift: a placeholder in the
/// shape of a code, as `BridgeSamples` records.
#[derive(Debug)]
pub struct FakeQrEncoder {
    pub png_base64: Mutex<Option<String>>,
}

impl FakeQrEncoder {
    #[must_use]
    pub fn new(png_base64: &str) -> Self {
        FakeQrEncoder {
            png_base64: Mutex::new(Some(png_base64.to_owned())),
        }
    }

    /// The PNG every text draws as; `None` when drawing fails.
    pub fn set_png(&self, png_base64: Option<&str>) {
        *lock(&self.png_base64) = png_base64.map(str::to_owned);
    }
}

impl QrEncoder for FakeQrEncoder {
    fn png_base64(&self, _text: &str) -> Option<String> {
        lock(&self.png_base64).clone()
    }
}

/// The input devices a test lists. Swift: `listInputs`.
#[derive(Debug, Default)]
pub struct FakeAudioDevices {
    pub devices: Mutex<Vec<InputDevice>>,
    pub failure: Mutex<Option<String>>,
}

impl FakeAudioDevices {
    pub fn set_devices(&self, devices: Vec<InputDevice>) {
        *lock(&self.devices) = devices;
    }

    /// Makes listing fail with `text`; `None` lists again.
    pub fn fail_listing(&self, text: Option<&str>) {
        *lock(&self.failure) = text.map(str::to_owned);
    }
}

impl AudioDevices for FakeAudioDevices {
    fn inputs(&self) -> BoundaryResult<Vec<InputDevice>> {
        if let Some(failure) = lock(&self.failure).clone() {
            return Err(failure.into());
        }
        Ok(lock(&self.devices).clone())
    }
}

/// The folder size a test sets, and whether the folder's drive may lose
/// recent writes. Swift: `measureFolder`.
#[derive(Debug)]
pub struct FakeFolderUsage {
    pub bytes: Mutex<Result<i64, String>>,
    pub measured: Mutex<Vec<PathBuf>>,
    pub may_lose_recent_writes: Mutex<bool>,
}

impl FakeFolderUsage {
    #[must_use]
    pub fn new(bytes: i64) -> Self {
        FakeFolderUsage {
            bytes: Mutex::new(Ok(bytes)),
            measured: Mutex::new(Vec::new()),
            may_lose_recent_writes: Mutex::new(false),
        }
    }

    /// What every measurement returns: the size, or the failure text.
    pub fn set_bytes(&self, bytes: Result<i64, &str>) {
        *lock(&self.bytes) = bytes.map_err(str::to_owned);
    }
}

impl FolderUsage for FakeFolderUsage {
    fn measure(&self, folder: &Path) -> BoundaryResult<i64> {
        lock(&self.measured).push(folder.to_path_buf());
        lock(&self.bytes).clone().map_err(Into::into)
    }

    fn may_lose_recent_writes(&self, _folder: &Path) -> bool {
        *lock(&self.may_lose_recent_writes)
    }
}

/// The files that "exist", and what the host removed. Swift:
/// `FileManager` over a temporary folder.
#[derive(Debug, Default)]
pub struct FakeFileSystem {
    pub existing: Mutex<BTreeSet<PathBuf>>,
    /// Every path `remove` was asked for, in order.
    pub removed: Mutex<Vec<PathBuf>>,
    /// Set to make every `remove` fail with this text, leaving the path.
    pub removal_failure: Mutex<Option<String>>,
}

impl FakeFileSystem {
    /// Makes `path` exist.
    pub fn create(&self, path: impl Into<PathBuf>) {
        lock(&self.existing).insert(path.into());
    }

    /// Makes every `remove` fail with `text`; `None` lets them through.
    pub fn fail_removals(&self, text: Option<&str>) {
        *lock(&self.removal_failure) = text.map(str::to_owned);
    }
}

impl FileSystem for FakeFileSystem {
    fn exists(&self, path: &Path) -> bool {
        lock(&self.existing).contains(path)
    }

    /// The path and everything under it stop existing.
    fn remove(&self, path: &Path) -> BoundaryResult<()> {
        lock(&self.removed).push(path.to_path_buf());
        if let Some(failure) = lock(&self.removal_failure).clone() {
            return Err(failure.into());
        }
        lock(&self.existing).retain(|existing| !existing.starts_with(path));
        Ok(())
    }
}

/// Plays whatever file the file system says exists. Swift: `ClipPlayer`
/// over a fake `AVAudioPlayer`.
pub struct FakeClipPlayer {
    file_system: Arc<dyn FileSystem>,
    playing: Mutex<Option<PathBuf>>,
    pub played: Mutex<Vec<PathBuf>>,
}

impl FakeClipPlayer {
    #[must_use]
    pub fn new(file_system: Arc<dyn FileSystem>) -> Self {
        FakeClipPlayer {
            file_system,
            playing: Mutex::new(None),
            played: Mutex::new(Vec::new()),
        }
    }
}

impl ClipPlayer for FakeClipPlayer {
    fn play(&self, clip: &Path) -> bool {
        *lock(&self.playing) = None;
        if !self.file_system.exists(clip) {
            return false;
        }
        lock(&self.played).push(clip.to_path_buf());
        *lock(&self.playing) = Some(clip.to_path_buf());
        true
    }

    fn stop(&self) {
        *lock(&self.playing) = None;
    }

    fn playing(&self) -> Option<PathBuf> {
        lock(&self.playing).clone()
    }
}

/// Records what the host asked to show. Swift: `NSWorkspace` and the
/// window actions.
#[derive(Debug, Default)]
pub struct FakeOpener {
    pub revealed: Mutex<Vec<PathBuf>>,
    pub opened: Mutex<Vec<String>>,
    pub windows_opened: Mutex<Vec<BridgeWindow>>,
    pub windows_closed: Mutex<Vec<BridgeWindow>>,
}

impl Opener for FakeOpener {
    fn reveal(&self, path: &Path) {
        lock(&self.revealed).push(path.to_path_buf());
    }

    fn open_url(&self, url: &str) {
        lock(&self.opened).push(url.to_owned());
    }

    fn open_window(&self, window: BridgeWindow) {
        lock(&self.windows_opened).push(window);
    }

    fn close_window(&self, window: BridgeWindow) {
        lock(&self.windows_closed).push(window);
    }
}

/// Flags in memory. Swift: a `UserDefaults` suite per test.
#[derive(Debug, Default)]
pub struct FakePreferences {
    pub flags: Mutex<BTreeMap<String, bool>>,
}

impl Preferences for FakePreferences {
    fn stored_flag(&self, key: &str) -> Option<bool> {
        lock(&self.flags).get(key).copied()
    }

    fn set_flag(&self, key: &str, value: bool) {
        lock(&self.flags).insert(key.to_owned(), value);
    }
}

/// Every fake at once, with a handle on each: what a test without a shell builds
/// its [`Services`] from. Swift: `AppEnvironment.preview()`.
pub struct FakeServices {
    pub clock: Arc<FakeClock>,
    pub login_item: Arc<FakeLoginItem>,
    pub permissions: Arc<FakePermissions>,
    pub updater: Arc<FakeUpdater>,
    pub recorder: Arc<FakeRecorder>,
    pub pipeline: Arc<FakePipeline>,
    pub speech_models: Arc<FakeSpeechModels>,
    pub llm: Arc<FakeLlmService>,
    pub export_validator: Arc<FakeExportValidator>,
    pub handover: Option<Arc<FakeHandover>>,
    pub qr: Arc<FakeQrEncoder>,
    pub audio_devices: Arc<FakeAudioDevices>,
    pub folder_usage: Arc<FakeFolderUsage>,
    pub file_system: Arc<FakeFileSystem>,
    pub clip_player: Arc<FakeClipPlayer>,
    pub opener: Arc<FakeOpener>,
    pub preferences: Arc<FakePreferences>,
    /// The core's own fake, behind the one core boundary the host consumes.
    pub secrets: Arc<InMemorySecretStore>,
}

impl FakeServices {
    /// Every permission granted, no login item, the updater never checked,
    /// no handover service, nothing installed, a fixed `now`.
    #[must_use]
    pub fn new(now: DateTime<Utc>) -> Self {
        let clock = Arc::new(FakeClock::new(now));
        let permissions = Arc::new(FakePermissions::all_granted());
        let file_system = Arc::new(FakeFileSystem::default());
        FakeServices {
            recorder: Arc::new(FakeRecorder::new(clock.clone(), permissions.clone())),
            clip_player: Arc::new(FakeClipPlayer::new(file_system.clone())),
            clock,
            login_item: Arc::new(FakeLoginItem::new(LoginItemStatus::NotRegistered)),
            permissions,
            updater: Arc::new(FakeUpdater::default()),
            pipeline: Arc::new(FakePipeline::default()),
            speech_models: Arc::new(FakeSpeechModels::default()),
            llm: Arc::new(FakeLlmService::default()),
            export_validator: Arc::new(FakeExportValidator::default()),
            handover: None,
            qr: Arc::new(FakeQrEncoder::new("")),
            audio_devices: Arc::new(FakeAudioDevices::default()),
            folder_usage: Arc::new(FakeFolderUsage::new(0)),
            file_system,
            opener: Arc::new(FakeOpener::default()),
            preferences: Arc::new(FakePreferences::default()),
            secrets: Arc::new(InMemorySecretStore::new()),
        }
    }

    /// Adds a handover listener with `mac_id` on `port`.
    #[must_use]
    pub fn with_handover(mut self, mac_id: &str, port: u16) -> Self {
        self.handover = Some(Arc::new(FakeHandover::new(
            mac_id,
            port,
            self.clock.clone(),
        )));
        self
    }

    /// The trait objects the host takes.
    #[must_use]
    pub fn services(&self) -> Services {
        Services {
            clock: self.clock.clone(),
            login_item: self.login_item.clone(),
            permissions: self.permissions.clone(),
            updater: self.updater.clone(),
            recorder: self.recorder.clone(),
            pipeline: self.pipeline.clone(),
            speech_models: self.speech_models.clone(),
            llm: self.llm.clone(),
            export_validator: self.export_validator.clone(),
            handover: self
                .handover
                .clone()
                .map(|handover| handover as Arc<dyn Handover>),
            qr: self.qr.clone(),
            audio_devices: self.audio_devices.clone(),
            folder_usage: self.folder_usage.clone(),
            file_system: self.file_system.clone(),
            clip_player: self.clip_player.clone(),
            opener: self.opener.clone(),
            preferences: self.preferences.clone(),
            secrets: self.secrets.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hook runs with none of the fake's locks held, so one that calls
    /// back into its fake (here: replaces itself) returns instead of
    /// deadlocking on the fake's own mutex.
    #[test]
    fn a_hook_may_call_its_fake_again() {
        let permissions = Arc::new(FakePermissions::all_granted());
        permissions.set_on_request({
            let permissions = Arc::downgrade(&permissions);
            move |_| {
                if let Some(permissions) = permissions.upgrade() {
                    permissions.set_on_request(|_| {});
                }
            }
        });
        let (sender, receiver) = std::sync::mpsc::channel();
        let requester = permissions.clone();
        std::thread::spawn(move || {
            let _ = sender.send(requester.request(PermissionKind::Calendar));
        });
        assert_eq!(
            receiver.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(PermissionState::Granted)
        );
    }
}
