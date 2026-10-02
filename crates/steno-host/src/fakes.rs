//! Fakes for every service in [`services`](crate::services), after
//! `Services/Fakes.swift` and the modules' `Testing/` fakes: each holds its
//! state behind a mutex, answers what a test set, and records what the
//! host asked, so the host runs hostless on a real store. The product never
//! constructs these; [`Services`] built from them is what the tests and the
//! CLI's dry run use.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use steno_bridge::{BridgeWindow, CaptureMode, PermissionKind, PermissionState, RecordingState};
use steno_core::testing::InMemorySecretStore;
use steno_core::{AudioRetention, HandoverReceipt, ObsidianSettings, PairedDevice, Settings};
use uuid::Uuid;

use crate::services::{
    AudioDevices, AutoStopStatus, ClipPlayer, Clock, CodexModel, CodexModelsError, ExportValidator,
    FileSystem, FolderUsage, Handover, InputDevice, LaneLevels, ListenerState, LlmService,
    LoginItem, LoginItemStatus, Opener, PairingCode, Permissions, Pipeline, Preferences, QrEncoder,
    Recorder, RecorderStatus, Services, SpeechModels, UpdateOutcome, Updater,
    permission_is_required,
};
use crate::speech::ModelAsset;

/// A poisoned fake is still readable: a panic in one test thread must not
/// hide the state the assertion wants to see.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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
}

impl LoginItem for FakeLoginItem {
    fn status(&self) -> LoginItemStatus {
        *lock(&self.status)
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        if let Some(failure) = lock(&self.failure).clone() {
            return Err(failure);
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
pub type RequestHook = Box<dyn Fn(PermissionKind) + Send + Sync>;

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

    pub fn set(&self, kind: PermissionKind, state: PermissionState) {
        lock(&self.states).insert(kind, state);
    }

    pub fn answer(&self, kind: PermissionKind, state: PermissionState) {
        lock(&self.answers).insert(kind, state);
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
        if let Some(hook) = lock(&self.on_request).as_ref() {
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
/// Swift: the `RecordingController` over the synthetic backend, reduced to
/// what the window reads.
pub struct FakeRecorder {
    status: Mutex<RecorderStatus>,
    clock: Arc<dyn Clock>,
    permissions: Arc<dyn Permissions>,
    pub starts: Mutex<Vec<(CaptureMode, Option<String>)>>,
    pub stops: Mutex<usize>,
    pub kept: Mutex<usize>,
}

impl FakeRecorder {
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, permissions: Arc<dyn Permissions>) -> Self {
        FakeRecorder {
            status: Mutex::new(RecorderStatus::idle()),
            clock,
            permissions,
            starts: Mutex::new(Vec::new()),
            stops: Mutex::new(0),
            kept: Mutex::new(0),
        }
    }

    /// Overwrites the status wholesale (a test staging a live recording).
    pub fn set_status(&self, status: RecorderStatus) {
        *lock(&self.status) = status;
    }

    pub fn set_levels(&self, levels: Option<LaneLevels>) {
        lock(&self.status).levels = levels;
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
        status.state = RecordingState::Recording;
        status.started_at = Some(self.clock.now());
        status.mode = Some(mode);
        status.call_app = call_app.map(str::to_owned);
        status.meeting_id = Some(Uuid::new_v4());
        status.warning = None;
        status.error = None;
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
}

/// Records every call; each operation fails with the text a test set.
/// Swift: the preview pipeline's fakes, reduced to the calls the view models
/// make.
#[derive(Debug, Default)]
pub struct FakePipeline {
    pub summary_reruns: Mutex<Vec<(Uuid, String)>>,
    pub redeliveries: Mutex<Vec<Uuid>>,
    pub retention: Mutex<Vec<(Uuid, AudioRetention)>>,
    pub reloads: Mutex<usize>,
    pub kept_forever: Mutex<i64>,
    pub failure: Mutex<Option<String>>,
}

impl FakePipeline {
    fn outcome(&self) -> Result<(), String> {
        lock(&self.failure).clone().map_or(Ok(()), Err)
    }
}

impl Pipeline for FakePipeline {
    fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> Result<(), String> {
        lock(&self.summary_reruns).push((meeting_id, template_id.to_owned()));
        self.outcome()
    }

    fn redeliver(&self, meeting_id: Uuid) -> Result<(), String> {
        lock(&self.redeliveries).push(meeting_id);
        self.outcome()
    }

    fn apply_retention(&self, meeting_id: Uuid, rule: AudioRetention) -> Result<(), String> {
        lock(&self.retention).push((meeting_id, rule));
        self.outcome()
    }

    fn reload(&self) -> Result<(), String> {
        *lock(&self.reloads) += 1;
        self.outcome()
    }

    fn keep_all_recordings(&self) -> Result<i64, String> {
        self.outcome()?;
        Ok(*lock(&self.kept_forever))
    }
}

/// Installed assets with their sizes; a download reports its `progress`
/// steps and installs. Swift: `ModelStore` over `FakeModelDownloader`.
#[derive(Debug)]
pub struct FakeSpeechModels {
    pub installed: Mutex<BTreeMap<ModelAsset, Option<i64>>>,
    pub downloads: Mutex<Vec<ModelAsset>>,
    pub removals: Mutex<Vec<ModelAsset>>,
    /// The `(fraction, phase)` reports of one download, in order.
    pub progress: Mutex<Vec<(f64, String)>>,
    pub download_failure: Mutex<Option<String>>,
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
        }
    }
}

impl FakeSpeechModels {
    pub fn install(&self, asset: ModelAsset, bytes: Option<i64>) {
        lock(&self.installed).insert(asset, bytes);
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
    ) -> Result<(), String> {
        lock(&self.downloads).push(asset);
        let steps = lock(&self.progress).clone();
        for (fraction, phase) in &steps {
            progress(*fraction, phase);
        }
        if let Some(failure) = lock(&self.download_failure).clone() {
            return Err(failure);
        }
        lock(&self.installed).insert(asset, Some(asset.approximate_bytes()));
        Ok(())
    }

    fn remove(&self, asset: ModelAsset) -> Result<(), String> {
        lock(&self.removals).push(asset);
        lock(&self.installed).remove(&asset);
        Ok(())
    }
}

/// Answers the probe and the Codex sign-in with what a test set.
#[derive(Debug)]
pub struct FakeLlmService {
    pub probe_result: Mutex<Result<String, String>>,
    pub codex_account: Mutex<Result<String, String>>,
    pub codex_models: Mutex<Result<Vec<CodexModel>, CodexModelsError>>,
    pub probes: Mutex<Vec<Settings>>,
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
        }
    }
}

impl LlmService for FakeLlmService {
    fn probe(&self, settings: &Settings, _api_key: Option<&str>) -> Result<String, String> {
        lock(&self.probes).push(settings.clone());
        lock(&self.probe_result).clone()
    }

    fn codex_account(&self) -> Result<String, String> {
        lock(&self.codex_account).clone()
    }

    fn codex_models(&self) -> Result<Vec<CodexModel>, CodexModelsError> {
        lock(&self.codex_models).clone()
    }
}

/// Accepts every vault unless a failure text is set.
#[derive(Debug, Default)]
pub struct FakeExportValidator {
    pub failure: Mutex<Option<String>>,
    pub validated: Mutex<Vec<ObsidianSettings>>,
}

impl ExportValidator for FakeExportValidator {
    fn validate(&self, settings: &ObsidianSettings) -> Result<(), String> {
        lock(&self.validated).push(settings.clone());
        lock(&self.failure).clone().map_or(Ok(()), Err)
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

    fn paired_devices(&self) -> Result<Vec<PairedDevice>, String> {
        Ok(lock(&self.devices).clone())
    }

    fn start(&self) -> Result<(), String> {
        *lock(&self.starts) += 1;
        if let Some(failure) = lock(&self.start_failure).clone() {
            *lock(&self.state) = ListenerState::Failed(failure.clone());
            return Err(failure);
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

    fn revoke(&self, device_id: Uuid) -> Result<(), String> {
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

impl AudioDevices for FakeAudioDevices {
    fn inputs(&self) -> Result<Vec<InputDevice>, String> {
        if let Some(failure) = lock(&self.failure).clone() {
            return Err(failure);
        }
        Ok(lock(&self.devices).clone())
    }
}

/// The folder size a test sets. Swift: `measureFolder`.
#[derive(Debug)]
pub struct FakeFolderUsage {
    pub bytes: Mutex<Result<i64, String>>,
    pub measured: Mutex<Vec<PathBuf>>,
}

impl FakeFolderUsage {
    #[must_use]
    pub fn new(bytes: i64) -> Self {
        FakeFolderUsage {
            bytes: Mutex::new(Ok(bytes)),
            measured: Mutex::new(Vec::new()),
        }
    }
}

impl FolderUsage for FakeFolderUsage {
    fn measure(&self, folder: &Path) -> Result<i64, String> {
        lock(&self.measured).push(folder.to_path_buf());
        lock(&self.bytes).clone()
    }
}

/// The files that "exist". Swift: `FileManager` over a temporary folder.
#[derive(Debug, Default)]
pub struct FakeFileSystem {
    pub existing: Mutex<BTreeSet<PathBuf>>,
}

impl FakeFileSystem {
    pub fn create(&self, path: impl Into<PathBuf>) {
        lock(&self.existing).insert(path.into());
    }

    pub fn remove(&self, path: &Path) {
        lock(&self.existing).remove(path);
    }
}

impl FileSystem for FakeFileSystem {
    fn exists(&self, path: &Path) -> bool {
        lock(&self.existing).contains(path)
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
    fn flag(&self, key: &str) -> bool {
        lock(&self.flags).get(key).copied().unwrap_or(false)
    }

    fn set_flag(&self, key: &str, value: bool) {
        lock(&self.flags).insert(key.to_owned(), value);
    }
}

/// Every fake at once, with a handle on each: what a hostless test builds
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
