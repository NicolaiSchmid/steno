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
//! text, once. The traits below stand in front of WP4 (speech models), WP5
//! (the recorder, the devices), WP6b's pipeline, WP7 (the LLM probe, the
//! vault validation, the handover listener) and the shell (permissions,
//! login item, updater, files, the Finder); each names the Swift type it
//! replaces.
//!
//! # Calling back into the host
//!
//! Most services are called with the host's view-model lock held: the
//! pipeline from the detail commands and every Save, the recorder's
//! `status` for each `recording` snapshot, the clip player's `playing`
//! while the detail is built, the handover, devices, folder usage, updater
//! and login item from the Settings commands, and the secret store when
//! the Settings sections reload. An implementation therefore must not
//! call back into the host (`store_changed`, `recorder_changed`,
//! `apply_meeting_event`, `phones_changed`, any command) from inside one of
//! its methods on the calling thread, and must not hold a lock of its own
//! while notifying the host if one of its methods takes that lock: the
//! first deadlocks on the host's mutex, the second in a lock-order cycle.
//! Notify from the service's own thread, after the method returned. The
//! recorder's commands, the permission prompts, the LLM probe, the Codex
//! calls and the model download run with the lock released and may block
//! as long as they need; the recorder's commands may also report their
//! change from inside (see [`Recorder`]).

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
/// Swift: `PermissionKind.deniedMessage` in
/// `apps/macos/Steno/Recording/RecordingControlPresentation.swift`.
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

/// The login item (`SMAppService` on the Mac; WP8 implements it per OS in
/// the shell). The fake records every `set_enabled` and each pane opened.
/// Swift: `LoginItemControlling`.
pub trait LoginItem: Send + Sync {
    fn status(&self) -> LoginItemStatus;
    fn set_enabled(&self, enabled: bool) -> BoundaryResult<()>;
    fn open_system_settings(&self);
}

/// TCC on the Mac, the portal or nothing elsewhere; WP8 implements it per
/// OS in the shell. The fake answers what a test set and records every
/// request and pane opened. Swift: `PermissionsChecking`.
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

/// Sparkle in the Swift app; in this one the update schedule over the
/// Tauri updater (`steno_services::updates`). The fake holds the flags and
/// counts the checks. Swift: `UpdaterControlling`.
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

/// The one recorder (WP5 wires the capture session behind it). A change
/// reaches the host through
/// [`Host::recorder_changed`](crate::Host::recorder_changed), never while
/// holding a lock `status` takes, since `status` is called with the host's
/// lock held (see the module doc). The commands are called with no host
/// lock held, so they may report their change from inside, on the calling
/// thread, as `steno_services`' capture recorder does. The fake runs the
/// state machine without a session and records every start, stop and
/// keep. Swift: `RecordingController`.
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
    /// Keeps `folder` among the folders crash recovery looks in for an
    /// interrupted recording's master: the settings' audio folder moves
    /// away from it, and a recording started there is written there to
    /// its end. A failure is logged, never refused. Rust only: Swift had no
    /// recovery.
    fn remember_audio_folder(&self, folder: &Path);
    /// What deleting meeting `meeting_id`, left `recording`, needs to know
    /// ([`LeftRecording`]). Rust only: Swift refused every recording row.
    fn left_recording(&self, meeting_id: Uuid) -> LeftRecording;
    /// The rows of meeting `meeting_id`, left `recording`, are gone: what
    /// the recorder kept for its recovery goes too. Rust only.
    fn forget_recording(&self, meeting_id: Uuid);
}

/// A meeting left `recording`, as [`Recorder::left_recording`] finds it
/// on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeftRecording {
    /// The audio folders its files may be in, each once: the one it was
    /// recorded into and the settings' one. A delete removes the meeting's
    /// folder in each.
    pub folders: Vec<PathBuf>,
    /// Its master in one of them was written within the last seconds, or
    /// at a time ahead of the clock: another process (the Swift app
    /// started after this one) may still be recording it, so the delete is
    /// refused.
    pub still_written: bool,
}

/// What the view models ask the processing pipeline and the retention
/// sweep for (WP6b implements it). Called with the host's lock held (see
/// the module doc): the pipeline reports its work through
/// [`Host::store_changed`](crate::Host::store_changed) and
/// [`Host::apply_meeting_event`](crate::Host::apply_meeting_event) from its
/// own thread, never from inside these methods, and never while holding a
/// lock these methods take. The fake records every call. Swift:
/// `ProcessingPipeline` and `RetentionSweep` through `AppEnvironment`.
pub trait Pipeline: Send + Sync {
    fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> BoundaryResult<()>;
    fn redeliver(&self, meeting_id: Uuid) -> BoundaryResult<()>;
    /// Whether the launch stopped re-exporting the meeting after its export
    /// failed at the pipeline's limit of launches in a row. The user's next
    /// export starts it again, and until then the detail says the export
    /// keeps failing. The default says no. Rust only.
    fn export_keeps_failing(&self, _meeting_id: Uuid) -> bool {
        false
    }
    fn apply_retention(&self, meeting_id: Uuid, rule: AudioRetention) -> BoundaryResult<()>;
    /// Rebuilds the pipeline from the stored settings and the API key.
    fn reload(&self) -> BoundaryResult<()>;
    /// `RetentionSweep.keepAll()`: marks every recording still on disk as
    /// kept forever and returns how many.
    fn keep_all_recordings(&self) -> BoundaryResult<i64>;
}

/// The speech model store (WP4). The fake keeps a map of installed
/// assets, records every download and removal, and reports the progress
/// steps a test set. Swift: `ModelStore` in
/// `Sources/StenoSpeech/Models/ModelStore.swift`.
pub trait SpeechModels: Send + Sync {
    fn is_installed(&self, asset: ModelAsset) -> bool;
    /// Whether every model the speech engine `engine_id` loads is on disk,
    /// so loading it starts no download. The default asks for the engine's
    /// asset; the services, which pick the engine per platform, answer for
    /// the engine they build. Swift: `models.isInstalled(engine.asset)` in
    /// `AppEnvironment.warmUpPipelineIfModelsInstalled`.
    fn engine_installed(&self, engine_id: &str) -> bool {
        engine_id
            .parse::<crate::speech::SpeechEngineId>()
            .is_ok_and(|engine| self.is_installed(engine.asset()))
    }
    fn installed_size(&self, asset: ModelAsset) -> Option<i64>;
    /// Downloads the asset, reporting `(fraction, phase)` as it goes;
    /// returns once installed.
    fn download(
        &self,
        asset: ModelAsset,
        progress: &mut dyn FnMut(f64, &str),
    ) -> BoundaryResult<()>;
    fn remove(&self, asset: ModelAsset) -> BoundaryResult<()>;

    /// The name the acknowledgements give `asset`. The model behind an
    /// asset is the services' choice per platform (the `CoreML` int8
    /// Parakeet where the Mac runs it, the fp32 ONNX export in the speech
    /// sidecar), so they may name it; the
    /// default is the Swift app's name, [`ModelAsset::display_name`].
    fn display_name(&self, asset: ModelAsset) -> &'static str {
        asset.display_name()
    }

    /// Where `asset`'s model comes from, for the acknowledgements; the
    /// default is the Swift app's repository, [`ModelAsset::source_repo`].
    fn source_repo(&self, asset: ModelAsset) -> &'static str {
        asset.source_repo()
    }

    /// About how many bytes `asset`'s model takes once installed, shown
    /// before a download and when the installed size cannot be read; the
    /// default is the Swift app's measure, [`ModelAsset::approximate_bytes`].
    /// The services override it where the model behind an asset is not the
    /// Swift app's (the fp32 export in the speech sidecar).
    fn expected_bytes(&self, asset: ModelAsset) -> i64 {
        asset.approximate_bytes()
    }
}

/// One entry of the Codex backend's model list. Swift: `CodexModel` in
/// `Sources/StenoLLM/Wire/Responses.swift`.
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

/// The LLM module's probe and the Codex sign-in (WP7). The fake answers
/// what a test set and records the settings of every probe. Swift:
/// `LLMWiring` and `CodexCredentialStore`.
pub trait LlmService: Send + Sync {
    /// One line for the Test button, or the failure text.
    fn probe(&self, settings: &Settings, api_key: Option<&str>) -> BoundaryResult<String>;
    /// The account line of the Codex sign-in on this computer, or why
    /// there is none. Reads the sign-in on disk, never the network: the
    /// Summaries section reads it with the host's lock held when it loads.
    /// Swift: `CodexCredentialStore.stored()`.
    fn codex_account(&self) -> BoundaryResult<String>;
    /// The Codex models on offer, listed ones only.
    fn codex_models(&self) -> Result<Vec<CodexModel>, CodexModelsError>;
}

/// Checks an Obsidian vault configuration without delivering anything
/// (WP7's `ObsidianFolderDestination.validate()`); the error text is the
/// destination's and is shown verbatim. The fake records every
/// configuration it checked and fails with the text a test set.
pub trait ExportValidator: Send + Sync {
    fn validate(&self, settings: &ObsidianSettings) -> BoundaryResult<()>;
}

/// Where the handover listener stands. Swift: `ListenerState` in
/// `Sources/StenoHandover/HandoverService.swift`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ListenerState {
    #[default]
    Stopped,
    Listening(u16),
    Failed(String),
}

/// An open pairing window: when it closes and what the QR code carries.
/// Swift: `PairingPayload` in `Sources/StenoHandover/Pairing/PairingPayload.swift`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    pub expires_at: DateTime<Utc>,
    pub url_string: String,
}

/// The Mac side of the phone handover (WP7). The fake counts starts and
/// stops, records every revoke and answers with the devices, receipts and
/// pairing code a test set. Swift: `HandoverService` in
/// `Sources/StenoHandover/HandoverService.swift`.
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

/// Draws a QR code as a PNG, base64 (`steno_services::qr` implements it).
/// The fake answers the image a test set, whatever the text. Swift:
/// `QRCode.png(for:)` in `apps/macos/Steno/Services/QRCode.swift`.
pub trait QrEncoder: Send + Sync {
    fn png_base64(&self, text: &str) -> Option<String>;
}

/// One input device. Swift: `AudioDeviceInfo` in
/// `Sources/StenoAudio/Capture/AudioDevices.swift`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    pub uid: String,
    pub name: String,
}

/// Lists the input devices (WP5, from the capture backend). The fake
/// answers the devices, or the failure, a test set. Swift:
/// `AudioDevices.inputs()` in `Sources/StenoAudio/Capture/AudioDevices.swift`.
pub trait AudioDevices: Send + Sync {
    fn inputs(&self) -> BoundaryResult<Vec<InputDevice>>;
}

/// Sums a folder (WP6b implements it over `std::fs`). Swift:
/// `AudioFolderUsage.measure` in
/// `Sources/StenoCore/Audio/AudioFolderUsage.swift`; the view model's
/// default treated a folder that does not exist as zero. The fake records
/// every folder it measured and answers the size a test set.
pub trait FolderUsage: Send + Sync {
    fn measure(&self, folder: &Path) -> BoundaryResult<i64>;
    /// Whether a power cut may lose a recording just written into `folder`
    /// (`steno_pipeline::files::may_lose_recent_writes`: a Windows drive
    /// that is neither NTFS nor `ReFS`, or a network drive or mount); the
    /// Swift app never asks.
    fn may_lose_recent_writes(&self, folder: &Path) -> bool;
}

/// The files the view models touch: whether one is on disk before offering
/// to play a clip or reveal a recording, and the removal of a deleted
/// meeting's files. [`RealFileSystem`] is the product's implementation;
/// the fake keeps a set of paths, so no clip or master has to exist.
/// Swift: `FileManager.fileExists` and `removeItem`.
pub trait FileSystem: Send + Sync {
    fn exists(&self, path: &Path) -> bool;
    /// Removes a file, or a folder with everything in it; a path that is
    /// not there is not an error.
    fn remove(&self, path: &Path) -> BoundaryResult<()>;
}

/// Plays a speaker's sample clip, one at a time (the shell implements it
/// in WP6b). The fake plays a clip the fake file system has and records
/// every clip it played. Swift: `ClipPlayer`.
///
/// No in-app playback while recording, enforced by `steno_audio::Playback`:
/// the call capture's tap includes Steno's own process, so the shell's
/// player asks `Playback::global().begin` before every clip, starts it
/// through the permit's `start`, and stops in the permit's `on_stop` when a
/// recording starts. The player is native; the web UI plays nothing (its
/// `<audio>` would play from the web view's media process, which the gate
/// cannot see).
pub trait ClipPlayer: Send + Sync {
    /// Starts the clip; false when the file is missing or unreadable. The
    /// shell's player, when it exists, returns false while a recording runs
    /// (`Playback::begin` refused).
    fn play(&self, clip: &Path) -> bool;
    fn stop(&self);
    fn playing(&self) -> Option<PathBuf>;
}

/// The Finder, the default browser and the shell's windows (the shell
/// implements it in WP6b). Swift:
/// `NSWorkspace` and the `openWindow` / `dismissWindow` actions the windows
/// installed on their bridges. The fake records every reveal, URL and
/// window opened or closed.
pub trait Opener: Send + Sync {
    fn reveal(&self, path: &Path);
    fn open_url(&self, url: &str);
    /// Shows the window (and brings the app forward); the request that
    /// names a meeting or a section rides on the `app` snapshot.
    fn open_window(&self, window: BridgeWindow);
    fn close_window(&self, window: BridgeWindow);
}

/// The flags the Swift app kept in `UserDefaults`: whether onboarding has
/// finished, whether the login item was registered once, and Sparkle's
/// automatic-check and automatic-download flags (the updater's). The shell
/// (WP6b) keeps them in its own settings file; the fake holds a map.
pub trait Preferences: Send + Sync {
    /// False when the key is missing or holds no boolean.
    fn flag(&self, key: &str) -> bool;
    /// `None` when the key is missing or holds no boolean, for a flag
    /// whose default is not false (the updater's automatic checks).
    fn stored_flag(&self, key: &str) -> Option<bool>;
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

    fn remove(&self, path: &Path) -> BoundaryResult<()> {
        let outcome = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        match outcome {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            outcome => Ok(outcome?),
        }
    }
}
