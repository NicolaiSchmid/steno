//! What the host reaches through a seam, after `Services/AppProtocols.swift`
//! and the module types `AppEnvironment.swift` injected as they were: one
//! trait per thing the Swift app got from a system framework or from a
//! package the Rust port has not written yet. Every trait is synchronous
//! and `Send + Sync`: the host runs blocking (see the crate doc), and a
//! real implementation that is itself async waits inside its method.
//!
//! The vocabulary is the bridge's where the two coincide
//! ([`PermissionKind`], [`PermissionState`], [`CaptureMode`],
//! [`RecordingState`]): the Swift app mapped its own enums onto the wire
//! names case by case, and the raw values are the same, so one type serves.
//!
//! Traits the core will own (`steno_core::protocols`) are not repeated
//! here: the API key goes through [`steno_core::SecretStore`]. Failures are
//! the core's [`BoundaryResult`] over `BoxError`, so a crate behind a seam
//! returns its own error type through `?` and the view models show it as
//! text, once. The traits
//! below stand in front of WP4 (speech models), WP5 (the recorder), WP6's
//! pipeline, WP7 (the LLM probe, the vault validation, the handover
//! listener) and the shell (permissions, login item, updater, devices,
//! files, the Finder); each names the Swift type it replaces.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use steno_bridge::{BridgeWindow, CaptureMode, PermissionKind, PermissionState, RecordingState};
use steno_core::protocols::BoundaryResult;
use steno_core::{AudioRetention, HandoverReceipt, ObsidianSettings, PairedDevice, Settings};
use uuid::Uuid;

use crate::speech::ModelAsset;

/// Recording needs the microphone and the system audio tap; calendar and
/// local network improve titles and enable the phone, and can wait.
/// Swift: `PermissionKind.isRequired`.
#[must_use]
pub fn permission_is_required(kind: PermissionKind) -> bool {
    matches!(
        kind,
        PermissionKind::Microphone | PermissionKind::SystemAudio
    )
}

/// The sentence a control shows when this permission is denied.
/// Swift: `PermissionKind.deniedMessage`.
#[must_use]
pub fn denied_message(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Microphone => "Microphone access is denied.",
        PermissionKind::SystemAudio => "System audio access is denied.",
        PermissionKind::Calendar => "Calendar access is denied.",
        PermissionKind::LocalNetwork => "Local network access is denied.",
    }
}

/// The one time source; tests pass a fixed date. Swift: `AppEnvironment.now`.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// `SMAppService.Status`, spelled without `ServiceManagement`.
/// Swift: `LoginItemStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginItemStatus {
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
}

impl LoginItemStatus {
    /// What a toggle shows: registered, whether or not the user has
    /// approved it in System Settings yet.
    #[must_use]
    pub fn is_on(self) -> bool {
        matches!(self, Self::Enabled | Self::RequiresApproval)
    }
}

/// Swift: `LoginItemControlling`.
pub trait LoginItem: Send + Sync {
    fn status(&self) -> LoginItemStatus;
    fn set_enabled(&self, enabled: bool) -> BoundaryResult<()>;
    fn open_system_settings(&self);
}

/// Swift: `PermissionsChecking`.
pub trait Permissions: Send + Sync {
    fn state(&self, kind: PermissionKind) -> PermissionState;
    /// Runs the system prompt (or the probe) and returns the resulting state.
    fn request(&self, kind: PermissionKind) -> PermissionState;
    /// Opens the System Settings pane where the user can change the answer.
    fn open_system_settings(&self, kind: PermissionKind);
}

/// What the last update check found. Swift: `UpdateCheckOutcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    NotChecked,
    UpToDate,
    Available(String),
    Failed(String),
}

/// Swift: `UpdaterControlling`.
pub trait Updater: Send + Sync {
    fn can_check_for_updates(&self) -> bool;
    fn automatically_checks(&self) -> bool;
    fn set_automatically_checks(&self, enabled: bool);
    /// Download and stage updates without asking; the install still waits
    /// for a relaunch.
    fn automatically_downloads(&self) -> bool;
    fn set_automatically_downloads(&self, enabled: bool);
    fn last_check_at(&self) -> Option<DateTime<Utc>>;
    fn last_outcome(&self) -> UpdateOutcome;
    fn check_for_updates(&self);
}

/// The armed auto-stop while a call recording waits out the grace after
/// the call app released the microphone. Swift: `AutoStop`.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoStopStatus {
    /// `None` when the app could not be named.
    pub app_name: Option<String>,
    pub remaining_seconds: f64,
    pub total_seconds: f64,
}

/// RMS per lane in `0...1`, `system` absent for an in-person recording.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneLevels {
    pub mic: f64,
    pub system: Option<f64>,
}

/// The recorder as the main window's `recording` topic reads it.
/// Swift: the observed properties of `RecordingController`.
#[derive(Debug, Clone, PartialEq)]
pub struct RecorderStatus {
    pub state: RecordingState,
    /// Set while `state` is `recording`.
    pub started_at: Option<DateTime<Utc>>,
    pub mode: Option<CaptureMode>,
    pub call_app: Option<String>,
    pub meeting_id: Option<Uuid>,
    pub levels: Option<LaneLevels>,
    pub auto_stop: Option<AutoStopStatus>,
    /// The required permissions the last refresh found denied.
    pub denied_permissions: Vec<PermissionKind>,
    pub warning: Option<String>,
    pub error: Option<String>,
}

impl RecorderStatus {
    /// Idle, nothing denied, no messages.
    #[must_use]
    pub fn idle() -> Self {
        RecorderStatus {
            state: RecordingState::Idle,
            started_at: None,
            mode: None,
            call_app: None,
            meeting_id: None,
            levels: None,
            auto_stop: None,
            denied_permissions: Vec::new(),
            warning: None,
            error: None,
        }
    }
}

/// The one recorder (WP5 wires the capture session behind it).
/// Swift: `RecordingController`.
pub trait Recorder: Send + Sync {
    fn status(&self) -> RecorderStatus;
    /// Starts a recording; `call_app` is the app the detection prompt named.
    fn start(&self, mode: CaptureMode, call_app: Option<&str>);
    fn stop(&self);
    fn toggle(&self);
    /// "Keep recording": disarms the auto-stop.
    fn keep_recording(&self);
    fn clear_messages(&self);
    /// Re-reads the required permissions into `denied_permissions`.
    fn refresh_permissions(&self);
}

/// What the view models ask the processing pipeline and the retention
/// sweep for. Swift: `ProcessingPipeline` and `RetentionSweep` through
/// `AppEnvironment`.
pub trait Pipeline: Send + Sync {
    fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> BoundaryResult<()>;
    fn redeliver(&self, meeting_id: Uuid) -> BoundaryResult<()>;
    fn apply_retention(&self, meeting_id: Uuid, rule: AudioRetention) -> BoundaryResult<()>;
    /// Rebuilds the pipeline from the stored settings and the API key.
    fn reload(&self) -> BoundaryResult<()>;
    /// `RetentionSweep.keepAll()`: marks every recording still on disk as
    /// kept forever and returns how many.
    fn keep_all_recordings(&self) -> BoundaryResult<i64>;
}

/// The speech model store (WP4). Swift: `ModelStore`.
pub trait SpeechModels: Send + Sync {
    fn is_installed(&self, asset: ModelAsset) -> bool;
    fn installed_size(&self, asset: ModelAsset) -> Option<i64>;
    /// Downloads the asset, reporting `(fraction, phase)` as it goes;
    /// returns once installed.
    fn download(
        &self,
        asset: ModelAsset,
        progress: &mut dyn FnMut(f64, &str),
    ) -> BoundaryResult<()>;
    fn remove(&self, asset: ModelAsset) -> BoundaryResult<()>;
}

/// One entry of the Codex backend's model list. Swift: `CodexModel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexModel {
    pub slug: String,
    pub display_name: String,
    pub context_window: Option<i64>,
}

/// Why the Codex model list failed. Swift: a `CodexCredentialError` versus
/// any other error from `LLMWiring.codexModels`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexModelsError {
    /// The sign-in itself is the problem; the card says so.
    Credential(String),
    Other(String),
}

/// The LLM module's probe and the Codex sign-in (WP7). Swift: `LLMWiring`
/// and `CodexCredentialStore`.
pub trait LlmService: Send + Sync {
    /// One line for the Test button, or the failure text.
    fn probe(&self, settings: &Settings, api_key: Option<&str>) -> BoundaryResult<String>;
    /// The account line of the Codex sign-in on this computer, or why
    /// there is none.
    fn codex_account(&self) -> BoundaryResult<String>;
    /// The Codex models on offer, listed ones only.
    fn codex_models(&self) -> Result<Vec<CodexModel>, CodexModelsError>;
}

/// Checks an Obsidian vault configuration without delivering anything
/// (WP7's `ObsidianFolderDestination.validate()`); the error text is the
/// destination's and is shown verbatim.
pub trait ExportValidator: Send + Sync {
    fn validate(&self, settings: &ObsidianSettings) -> BoundaryResult<()>;
}

/// Where the handover listener stands. Swift: `ListenerState`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ListenerState {
    #[default]
    Stopped,
    Listening(u16),
    Failed(String),
}

/// An open pairing window: when it closes and what the QR code carries.
/// Swift: `PairingPayload`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    pub expires_at: DateTime<Utc>,
    pub url_string: String,
}

/// The Mac side of the phone handover (WP7). Swift: `HandoverService`.
pub trait Handover: Send + Sync {
    fn state(&self) -> ListenerState;
    /// The identity's id as the page shows it.
    fn mac_id(&self) -> String;
    fn paired_devices(&self) -> BoundaryResult<Vec<PairedDevice>>;
    fn start(&self) -> BoundaryResult<()>;
    fn stop(&self);
    fn begin_pairing(&self) -> PairingCode;
    fn cancel_pairing(&self);
    fn revoke(&self, device_id: Uuid) -> BoundaryResult<()>;
    /// Every receipt the listener knows, any state.
    fn receipts(&self) -> Vec<HandoverReceipt>;
}

/// Draws a QR code as a PNG, base64. Swift: `QRCode.png(for:)`.
pub trait QrEncoder: Send + Sync {
    fn png_base64(&self, text: &str) -> Option<String>;
}

/// One input device. Swift: `AudioDeviceInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    pub uid: String,
    pub name: String,
}

/// Lists the input devices. Swift: `AudioDevices.inputs()`.
pub trait AudioDevices: Send + Sync {
    fn inputs(&self) -> BoundaryResult<Vec<InputDevice>>;
}

/// Sums a folder. Swift: `AudioFolderUsage.measure`; the view model's
/// default treated a folder that does not exist as zero.
pub trait FolderUsage: Send + Sync {
    fn measure(&self, folder: &Path) -> BoundaryResult<i64>;
}

/// Whether a file is on disk; the view models ask before offering to play
/// a clip or reveal a recording. Swift: `FileManager.fileExists`.
pub trait FileSystem: Send + Sync {
    fn exists(&self, path: &Path) -> bool;
}

/// Plays a speaker's sample clip, one at a time. Swift: `ClipPlayer`.
pub trait ClipPlayer: Send + Sync {
    /// Starts the clip; false when the file is missing or unreadable.
    fn play(&self, clip: &Path) -> bool;
    fn stop(&self);
    fn playing(&self) -> Option<PathBuf>;
}

/// The Finder, the default browser and the shell's windows. Swift:
/// `NSWorkspace` and the `openWindow` / `dismissWindow` actions the windows
/// installed on their bridges.
pub trait Opener: Send + Sync {
    fn reveal(&self, path: &Path);
    fn open_url(&self, url: &str);
    /// Shows the window (and brings the app forward); the request that
    /// names a meeting or a section rides on the `app` snapshot.
    fn open_window(&self, window: BridgeWindow);
    fn close_window(&self, window: BridgeWindow);
}

/// The two flags the Swift app kept in `UserDefaults`: whether onboarding
/// has finished and whether the login item was registered once.
pub trait Preferences: Send + Sync {
    fn flag(&self, key: &str) -> bool;
    fn set_flag(&self, key: &str, value: bool);
}

/// Everything the host is handed at construction, one `Arc` each so a test
/// keeps a handle on the fake it reads back. Swift: `AppEnvironment`.
#[derive(Clone)]
pub struct Services {
    pub clock: Arc<dyn Clock>,
    pub login_item: Arc<dyn LoginItem>,
    pub permissions: Arc<dyn Permissions>,
    pub updater: Arc<dyn Updater>,
    pub recorder: Arc<dyn Recorder>,
    pub pipeline: Arc<dyn Pipeline>,
    pub speech_models: Arc<dyn SpeechModels>,
    pub llm: Arc<dyn LlmService>,
    pub export_validator: Arc<dyn ExportValidator>,
    /// `None` when the handover identity could not be created; the Phones
    /// settings say so.
    pub handover: Option<Arc<dyn Handover>>,
    pub qr: Arc<dyn QrEncoder>,
    pub audio_devices: Arc<dyn AudioDevices>,
    pub folder_usage: Arc<dyn FolderUsage>,
    pub file_system: Arc<dyn FileSystem>,
    pub clip_player: Arc<dyn ClipPlayer>,
    pub opener: Arc<dyn Opener>,
    pub preferences: Arc<dyn Preferences>,
    pub secrets: Arc<dyn steno_core::SecretStore>,
}

/// `std::fs` as the [`FileSystem`]: the product's implementation, which
/// tests replace with a fake so no clip or master has to exist.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFileSystem;

impl FileSystem for RealFileSystem {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}
