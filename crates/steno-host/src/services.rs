//! The seams the host reaches its services through: one synchronous
//! `Send + Sync` trait per boundary (a real async implementation waits
//! inside its method). Failures are the core's [`BoundaryResult`] over
//! `BoxError`, shown by the view models as text.
//!
//! # Calling back into the host
//!
//! Most services are called with the host's view-model lock held, so an
//! implementation must not call back into the host (`store_changed`,
//! `recorder_changed`, `apply_meeting_event`, `phones_changed`, any
//! command) from inside a method on the calling thread, nor hold a lock of
//! its own while notifying the host if one of its methods takes that lock.
//! Notify from the service's own thread. The recorder's commands, the
//! permission prompts, the LLM probe, the Codex calls and the model
//! download run with the lock released; the recorder's commands may also
//! report their change from inside (see [`Recorder`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use steno_bridge::{
    BridgeWindow, CaptureMode, PermissionKind, PermissionState, PhoneWaitingReason, Platform,
    RecordingState,
};
use steno_core::protocols::BoundaryResult;
use steno_core::{AudioRetention, HandoverReceipt, ObsidianSettings, PairedDevice, Settings};
use uuid::Uuid;

use crate::speech::ModelAsset;

/// Recording needs the microphone and the system audio tap; calendar and
/// local network improve titles and enable the phone, and can wait.
#[must_use]
pub fn permission_is_required(kind: PermissionKind) -> bool {
    matches!(
        kind,
        PermissionKind::Microphone | PermissionKind::SystemAudio
    )
}

/// Whether a required permission in `state` still needs the user on
/// `platform` (opens onboarding, holds page 1's Continue, sets the
/// Recording subtitle): `denied` everywhere, and `unknown` on the Mac,
/// where it means not asked or not probed yet. Off the Mac `unknown` is
/// all the system says before a recording, so it does not count.
#[must_use]
pub fn permission_is_missing(platform: Platform, state: PermissionState) -> bool {
    match state {
        PermissionState::Granted => false,
        PermissionState::Denied => true,
        PermissionState::Unknown => platform == Platform::Macos,
    }
}

/// The sentence a control shows when this permission is denied.
#[must_use]
pub fn denied_message(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Microphone => "Microphone access is denied.",
        PermissionKind::SystemAudio => "System audio access is denied.",
        PermissionKind::Calendar => "Calendar access is denied.",
        PermissionKind::LocalNetwork => "Local network access is denied.",
    }
}

/// The one time source; tests pass a fixed date.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// The login item's status, after `SMAppService.Status`, plus `Managed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginItemStatus {
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
    /// The system starts the app at login and owns the setting
    /// (`STENO_LOGIN_ITEM=managed`, set by the NixOS module): the host never
    /// changes it, and General shows it on and locked.
    Managed,
}

impl LoginItemStatus {
    /// What a toggle shows: registered (approved or not yet), or started
    /// by the system.
    #[must_use]
    pub fn is_on(self) -> bool {
        matches!(self, Self::Enabled | Self::RequiresApproval | Self::Managed)
    }
}

/// The login item (`SMAppService` on the Mac, per OS in the shell).
pub trait LoginItem: Send + Sync {
    fn status(&self) -> LoginItemStatus;
    fn set_enabled(&self, enabled: bool) -> BoundaryResult<()>;
    fn open_system_settings(&self);
}

/// The OS permissions: TCC on the Mac, the portal or nothing elsewhere.
pub trait Permissions: Send + Sync {
    fn state(&self, kind: PermissionKind) -> PermissionState;
    /// Runs the system prompt (or the probe) and returns the resulting state.
    fn request(&self, kind: PermissionKind) -> PermissionState;
    /// Opens the System Settings pane where the user can change the answer.
    fn open_system_settings(&self, kind: PermissionKind);
}

/// What the last update check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    NotChecked,
    UpToDate,
    Available(String),
    Failed(String),
}

/// The update schedule over the Tauri updater (`steno_services::updates`).
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
/// the call app released the microphone.
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

/// The one recorder. A change reaches the host through
/// [`Host::recorder_changed`](crate::Host::recorder_changed), never while
/// holding a lock `status` takes: the hook also reads `status`, and the
/// reload pass calls it under the host's lock. The commands are called
/// with no host lock held, so they may report their change from inside.
pub trait Recorder: Send + Sync {
    fn status(&self) -> RecorderStatus;
    /// Starts a recording; `call_app` is the app the detection prompt named.
    /// While a [`StartHold`] lives the start is refused, and the status's
    /// error says [`INSTALLING_UPDATE`].
    fn start(&self, mode: CaptureMode, call_app: Option<&str>);
    /// Holds every recording start off until the hold is dropped, for an
    /// install that relaunches the app. A recording already under way goes
    /// on; the hold is taken under the lock a start checks, so
    /// [`Self::status`] read after it says what an install would stop.
    /// Dropping the last hold clears the refusal's error.
    fn hold_starts(&self) -> StartHold;
    /// Refuses every recording start from now on ([`QUITTING_REFUSAL`]):
    /// the menu Quit calls it before waiting for earlier actions, so no
    /// surface leaves a recording to save on the way out. A recording
    /// already under way goes on; a system audio test is stopped. Takes no
    /// lock a command holds while it waits, so the main thread may call it.
    fn refuse_starts(&self);
    fn stop(&self);
    fn toggle(&self);
    /// "Keep recording": disarms the auto-stop.
    fn keep_recording(&self);
    fn clear_messages(&self);
    /// Re-reads the required permissions into `denied_permissions`.
    fn refresh_permissions(&self);
    /// Keeps `folder` among the folders crash recovery looks in, after the
    /// settings' audio folder moved away from it. A failure is logged.
    fn remember_audio_folder(&self, folder: &Path);
    /// What deleting `meeting_id` needs to know: the audio folders its
    /// folder may be in, and for one left `recording` whether its master is
    /// still written. Asked only for a meeting left `recording` or one
    /// whose files no asset names.
    fn left_recording(&self, meeting_id: Uuid) -> LeftRecording;
    /// Before a confirmed delete's rows go, forgets what the recorder kept
    /// for the meeting's recovery, so the launch does not adopt a master
    /// whose removal failed. Returns the folder it forgot, recorded again
    /// ([`Self::restore_recording`]) when the delete does not go through.
    /// On an error the host refuses the delete; when the write failed after
    /// its rename, the store is checkpointed durably instead.
    fn forget_recording(&self, meeting_id: Uuid) -> std::io::Result<Option<PathBuf>>;
    /// Records `folder` again for `meeting_id` after a delete that did not
    /// go through: until the row is durable, the entry is what lets the
    /// next launch adopt its master. On failure the store is checkpointed
    /// durably; both are logged.
    fn restore_recording(&self, meeting_id: Uuid, folder: &Path);
}

/// What [`Recorder::hold_starts`] returns: recording starts are refused
/// until it is dropped.
pub type StartHold = Box<dyn Send>;

/// The error a start refused under a [`StartHold`] leaves in the status.
/// The hold is kept only while Steno shuts down to relaunch (and on
/// Windows while it starts the installer), never during a password prompt.
pub const INSTALLING_UPDATE: &str =
    "Steno is relaunching to finish installing an update. You can record again in a moment.";

/// The error a start refused after [`Recorder::refuse_starts`] leaves in
/// the status: the app is quitting.
pub const QUITTING_REFUSAL: &str = "Steno is quitting. Open it again to record.";

/// A meeting about to be deleted, as [`Recorder::left_recording`] finds it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeftRecording {
    /// The audio folders its files may be in, each once. A delete removes
    /// the meeting's folder in each when no asset names it, so the next
    /// launch does not adopt its master as a recording with no meeting.
    pub folders: Vec<PathBuf>,
    /// Its master was written within the last seconds, or ahead of the
    /// clock: another process may still record it, so the delete is refused.
    pub still_written: bool,
}

/// The processing pipeline and the retention sweep. Called with the host's
/// lock held: the pipeline reports through
/// [`Host::apply_meeting_event`](crate::Host::apply_meeting_event) and
/// [`Host::request_reload`](crate::Host::request_reload) from its own
/// thread, never from inside these methods.
pub trait Pipeline: Send + Sync {
    fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> BoundaryResult<()>;
    fn redeliver(&self, meeting_id: Uuid) -> BoundaryResult<()>;
    /// "Process again": saves the meeting queued and starts its run from
    /// `decode`, returning once it started. Refused without a change for
    /// the reason [`ProcessAgainRefusal`] gives, including a stored meeting
    /// that is not offered ([`steno_core::Meeting::offers_process_again`]).
    fn process_again(&self, meeting_id: Uuid) -> Result<(), ProcessAgainRefusal>;
    fn apply_retention(&self, meeting_id: Uuid, rule: AudioRetention) -> BoundaryResult<()>;
    /// Rebuilds the pipeline from the stored settings and the API key.
    fn reload(&self) -> BoundaryResult<()>;
    /// Marks every recording still on disk as kept forever and returns how
    /// many.
    fn keep_all_recordings(&self) -> BoundaryResult<i64>;
}

/// Why [`Pipeline::process_again`] would not process a meeting again, one
/// variant per answer the detail words differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessAgainRefusal {
    /// No meeting has the id.
    MeetingGone,
    /// The meeting is recording or ready
    /// ([`steno_core::Meeting::offers_process_again`]).
    NotOffered,
    /// The master is not on disk, or the meeting has no recording on
    /// record.
    RecordingGone,
    /// The meeting is queued or processing, or another operation holds
    /// it.
    Busy,
    /// The detail's own: the API key is withheld ([`WithheldApiKey`]), so
    /// the run would have no cleanup and no summary.
    KeyWithheld,
    /// The meeting waits for missing models
    /// ([`ProgressEntry::MODELS_MISSING_TITLE`](crate::main_window::ProgressEntry::MODELS_MISSING_TITLE)).
    ModelsMissing,
    /// The meeting waits for a store another process held
    /// ([`ProgressEntry::STORE_BUSY_TITLE`](crate::main_window::ProgressEntry::STORE_BUSY_TITLE)).
    StoreBusy,
    /// The app is exiting; the next launch can process the meeting again.
    Quitting,
    /// The store failed reading or queueing the meeting: the failure's text.
    CouldNotStart(String),
}

/// The speech model store.
pub trait SpeechModels: Send + Sync {
    fn is_installed(&self, asset: ModelAsset) -> bool;
    /// Whether every model engine `engine_id` loads is on disk, so loading
    /// it starts no download. The services answer for the engine they
    /// build per platform.
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

    /// The name the acknowledgements give `asset`; the services may name
    /// the model they pick per platform. Defaults to
    /// [`ModelAsset::display_name`].
    fn display_name(&self, asset: ModelAsset) -> &'static str {
        asset.display_name()
    }

    /// Where `asset`'s model comes from; defaults to
    /// [`ModelAsset::source_repo`].
    fn source_repo(&self, asset: ModelAsset) -> &'static str {
        asset.source_repo()
    }

    /// The acknowledgement lines of `asset`, one per model behind it; the
    /// default is one line of [`Self::display_name`],
    /// [`ModelAsset::licence`] and [`Self::source_repo`].
    fn notices(&self, asset: ModelAsset) -> Vec<ModelNotice> {
        vec![ModelNotice {
            name: self.display_name(asset).to_owned(),
            licence: asset.licence().to_owned(),
            source: self.source_repo(asset).to_owned(),
        }]
    }

    /// About how many bytes `asset`'s model takes once installed; defaults
    /// to [`ModelAsset::approximate_bytes`].
    fn expected_bytes(&self, asset: ModelAsset) -> i64 {
        asset.approximate_bytes()
    }
}

/// One line of the Acknowledgements dialog for a model: what it is and who
/// made it, its licence, and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelNotice {
    pub name: String,
    pub licence: String,
    pub source: String,
}

/// One entry of the Codex backend's model list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexModel {
    pub slug: String,
    pub display_name: String,
    pub context_window: Option<i64>,
}

/// Why the Codex model list failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexModelsError {
    /// The sign-in itself is the problem; the card says so.
    Credential(String),
    Other(String),
}

/// The LLM probe and the Codex sign-in.
pub trait LlmService: Send + Sync {
    /// One line for the Test button, or the failure text.
    fn probe(&self, settings: &Settings, api_key: Option<&str>) -> BoundaryResult<String>;
    /// The account line of the Codex sign-in on this computer, or why there
    /// is none. Reads the sign-in on disk, never the network.
    fn codex_account(&self) -> BoundaryResult<String>;
    /// The Codex models on offer, listed ones only.
    fn codex_models(&self) -> Result<Vec<CodexModel>, CodexModelsError>;
}

/// Checks an Obsidian vault configuration without delivering anything;
/// the refusal's text is shown verbatim.
pub trait ExportValidator: Send + Sync {
    fn validate(&self, settings: &ObsidianSettings) -> Result<(), VaultRefusal>;
}

/// Why [`ExportValidator::validate`] refused a vault.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct VaultRefusal {
    /// The destination's words.
    pub message: String,
    /// The folder is there but refused the write (on the Mac, most often
    /// Files and Folders). False when the system setting would not fix it.
    pub not_writable: bool,
}

/// Where the handover listener stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ListenerState {
    #[default]
    Stopped,
    Listening(u16),
    Failed(String),
}

/// An open pairing window: when it closes and what the QR code carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    pub expires_at: DateTime<Utc>,
    pub url_string: String,
}

/// The computer's side of the phone handover.
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
    /// Why the listener is off and waits for [`Self::try_again`] or
    /// [`Self::pair_again`]: the identity the paired phones know could not
    /// be read, and none is minted over it. `None` when it does not wait.
    fn waiting(&self) -> Option<PhoneWaitingReason>;
    /// Reads the identity again without a restart; the listener comes up
    /// when the read succeeds. May wait on a keychain prompt, so the host
    /// calls it with its lock released.
    fn try_again(&self) -> BoundaryResult<()>;
    /// Removes every paired phone and their identity and makes a new one.
    /// On the Mac the import's `swift-import-done` marker is written first,
    /// and a marker that cannot be written stops it. May wait on a keychain
    /// prompt.
    fn pair_again(&self) -> BoundaryResult<()>;
}

/// Draws a QR code as a base64 PNG.
pub trait QrEncoder: Send + Sync {
    fn png_base64(&self, text: &str) -> Option<String>;
}

/// One input device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    pub uid: String,
    pub name: String,
}

/// Lists the input devices.
pub trait AudioDevices: Send + Sync {
    fn inputs(&self) -> BoundaryResult<Vec<InputDevice>>;
}

/// Sums a folder.
pub trait FolderUsage: Send + Sync {
    fn measure(&self, folder: &Path) -> BoundaryResult<i64>;
}

/// The files the view models touch. [`RealFileSystem`] is the product's;
/// the fake keeps a set of paths.
pub trait FileSystem: Send + Sync {
    fn exists(&self, path: &Path) -> bool;
    /// Removes a file, or a folder with everything in it; a path that is
    /// not there is not an error.
    fn remove(&self, path: &Path) -> BoundaryResult<()>;
}

/// Plays a speaker's sample clip, one at a time.
///
/// No in-app playback while recording, enforced by `steno_audio::Playback`:
/// the call capture's tap includes Steno's own process, so the player asks
/// `Playback::global().begin` before every clip and stops in the permit's
/// `on_stop` when a recording starts. The web UI plays nothing: its
/// `<audio>` would play from a process the gate cannot see.
pub trait ClipPlayer: Send + Sync {
    /// Starts the clip; false when the file is missing or unreadable, or
    /// while a recording runs.
    fn play(&self, clip: &Path) -> bool;
    fn stop(&self);
    fn playing(&self) -> Option<PathBuf>;
}

/// The file manager, the default browser and the shell's windows.
pub trait Opener: Send + Sync {
    fn reveal(&self, path: &Path);
    fn open_url(&self, url: &str);
    /// Shows the window (and brings the app forward); the request that
    /// names a meeting or a section rides on the `app` snapshot.
    fn open_window(&self, window: BridgeWindow);
    fn close_window(&self, window: BridgeWindow);
}

/// The app's boolean preferences: onboarding finished, the login item
/// registered once, the updater's automatic flags.
pub trait Preferences: Send + Sync {
    /// False when the key is missing or holds no boolean.
    fn flag(&self, key: &str) -> bool {
        self.stored_flag(key).unwrap_or(false)
    }
    /// `None` when the key is missing or holds no boolean, for a flag
    /// whose default is not false (the updater's automatic checks).
    fn stored_flag(&self, key: &str) -> Option<bool>;
    fn set_flag(&self, key: &str, value: bool);
}

/// Where the import of the Swift app's keychain items stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwiftImportStage {
    /// The onboarding step has not run at this launch.
    Pending,
    /// The handover identity did not come over: phone handover waits until
    /// the step runs again.
    Waiting,
    /// The identity is in place, the key read or left empty.
    Done,
}

/// The import step's state for the onboarding page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwiftImportStatus {
    pub stage: SwiftImportStage,
    /// The keychain prompts a run may bring up, at most three: one per item
    /// it still has to read.
    pub prompts: u8,
    /// Why the last run left the identity behind, for the step's line.
    pub error: Option<String>,
}

/// The import of the Swift app's API key and handover identity on the
/// Mac's first launch after the update, behind the onboarding step. `None`
/// in [`Services`] when there is nothing to import.
pub trait SwiftImport: Send + Sync {
    fn status(&self) -> SwiftImportStatus;
    /// Reads the key (once) and exports the identity. Blocks on keychain
    /// prompts, so the host calls it with its lock released. While another
    /// run is under way it answers the status at once.
    fn run(&self) -> SwiftImportStatus;
    /// Not now, or the window closed over the step: nothing more is read
    /// this launch and phone handover waits. Never waits for a run under
    /// way.
    fn skip(&self) -> SwiftImportStatus;
}

/// Whether the API key is withheld from the pipeline now: the Swift
/// import's gate before its step ran or after it was skipped or refused,
/// until a key is saved. `None` in [`Services`] without such a gate.
pub trait WithheldApiKey: Send + Sync {
    /// Whether the key is withheld for an endpoint that needs it; the
    /// detail then says why a summary was skipped. `ChatGPT` summaries need
    /// no key, so for them this is false.
    fn withheld(&self, settings: &Settings) -> bool;
}

/// Everything the host is handed at construction, one `Arc` each so a test
/// keeps a handle on its fake.
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
    /// `None` when the handover could not be built at launch. One whose
    /// identity cannot be read is there and waits ([`Handover::waiting`]).
    pub handover: Option<Arc<dyn Handover>>,
    pub qr: Arc<dyn QrEncoder>,
    pub audio_devices: Arc<dyn AudioDevices>,
    pub folder_usage: Arc<dyn FolderUsage>,
    pub file_system: Arc<dyn FileSystem>,
    pub clip_player: Arc<dyn ClipPlayer>,
    pub opener: Arc<dyn Opener>,
    pub preferences: Arc<dyn Preferences>,
    pub secrets: Arc<dyn steno_core::SecretStore>,
    /// The onboarding step of the Swift import, while it has work to do.
    pub swift_import: Option<Arc<dyn SwiftImport>>,
    /// What may withhold the API key, while there is such a gate.
    pub withheld_api_key: Option<Arc<dyn WithheldApiKey>>,
}

/// `std::fs` as the [`FileSystem`].
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
