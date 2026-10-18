//! The bridge host over the real store: one [`Host`] owns the view models
//! of the three windows and answers every bridge method. Commands map one
//! to one onto the view models' methods; destructive ones ask the shell's
//! `confirm` first, folder choosers go through `choose_folder`.
//!
//! # Which window is calling
//!
//! The shell answers each window through [`Host::for_window`]. The
//! Summaries methods (`settings.summaries.*`) are sent by both the Settings
//! and the onboarding window, and each answers them on its own
//! `LlmSettingsViewModel`. A host never bound answers as the main window.
//!
//! # Publishing
//!
//! Every command publishes the topics it changed before it returns, except
//! `settings.transcription.download`, which keeps publishing from the
//! download's thread. Snapshots are built under the host's lock and emitted
//! after it is released, in build order, so a sink may read
//! [`Host::snapshot`] from inside `emit` but must not run a command there.
//! The `recording` topic comes from its own thread ([`recording_feed`]).
//!
//! # Reloads
//!
//! The services' poll task runs [`Host::reload_pass`] on its tick and on
//! each [`Host::request_reload`]. A pass always runs its light part and
//! reloads rows and settings only when asked or when the store's change
//! mark ([`Store::change_mark`]) moved. [`Host::store_changed`] runs a
//! forced pass on the caller's thread.
//!
//! # Threads
//!
//! The shell calls from its blocking pool, never from the thread marked by
//! [`mark_ui_thread`](crate::mark_ui_thread). The event follower and the
//! reload owner call from their runtime's blocking pool, so a reload that
//! waits never holds a runtime worker. Under the lock the host reads on a
//! reader of its own ([`Store::with_own_reader`]). The launch
//! (`steno_services::App::launch`) is the one exemption: it runs on the
//! main thread during setup and also writes (resuming unfinished work, the
//! retention sweep).
//!
//! Locks are taken in one order: the publishing mutex, then the view
//! models, then the store's writer, then a reader (shared, then the
//! host's; nothing is taken under a reader except by the checkpoint), and
//! last the leaves, each taken alone and briefly: the sink slot, the
//! recorder's status lock, the secret store's read. The rules it needs:
//!
//! - The `recording` feed and the recorder's hook take no host lock.
//! - Under the view models nothing runs a publish, a pass,
//!   [`Host::store_changed`], [`Host::recorder_changed`] or a command.
//!   Asking for a reload only sets a flag.
//! - The recorder calls its hook only with its own locks released.
//! - No `Store` call inside a `Store` closure, and the writer's commit hook
//!   takes no lock and calls nothing.
//!
//! The dialogs are called with no lock held. The services are called with
//! the view-model lock held, so they must not call back into the host,
//! except for the calls made with it released: the recorder's commands,
//! the permission prompts, the LLM probe, the Codex calls, the model
//! download and onboarding's vault check. The secret store is awaited on
//! the host's own runtime, or on a helper thread when the call arrives
//! inside a tokio runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock, RwLockReadGuard};
use std::thread;

use chrono::{DateTime, FixedOffset, Utc};
use serde_json::{Value, to_value};
use steno_bridge::{
    AppPhone, AssetIdParams, BridgeError, BridgeEvent, BridgeHost, BridgeTopic, BridgeWindow,
    ChosenPathReply, ConfirmDestructiveParams, ConfirmReply, DeviceIdParams, EventSink,
    ExportUpdateParams, MeetingIdParams, OpenUrlParams, Outcome, PageLayoutParams, PermissionKind,
    PermissionKindParams, PermissionState, Platform, RecordingState, SaveNotesParams,
    SelectSpeakerParams, SetAutomaticUpdatesParams, SetBoolParams, SetFilterParams, SetQueryParams,
    SetRetentionParams, SetStringParams, SetTabParams, SetTagFilterParams, SetTagsParams,
    SetTemplateParams, SettingsSection, SetupStepParams, SpeakerIdParams, SpeakerOption,
    SpeakerOptionKind, SpeakerOptionsParams, SpeakerOptionsReply, StartRecordingParams,
    SummariesUpdateParams, WindowParams,
};
use steno_core::paths::file_url_path;
use steno_core::protocols::SecretKey;
use steno_core::{ChangeMark, Store, StoreError};
use uuid::Uuid;

use crate::main_window::progress::MeetingEvent;
use crate::main_window::snapshots::{self as main_snapshots, AppState};
use crate::main_window::{
    MeetingDetailViewModel, MeetingListViewModel, ProcessingProgressModel, detail,
};
use crate::onboarding::{self, OnboardingViewModel};
use crate::publisher::TopicPublisher;
use crate::recording_feed::{self, RecordingFeed};
use crate::services::{RecorderStatus, Services, VaultRefusal};
use crate::settings::{
    AudioSettingsViewModel, GeneralSettingsViewModel, KeyRead, LlmPreset, LlmSettingsViewModel,
    ObsidianSettingsViewModel, PhonesSettingsViewModel, SpeechSettingsViewModel, llm::Probe,
    obsidian::VaultSave, overview, snapshots as settings_snapshots,
};
use crate::setup::engine_notice;
use crate::speakers::{OptionKind, SpeakerOption as ModelOption};
use crate::speech::{ModelAsset, SpeechEngineId};
use crate::tray::{self, TrayMeetings};

/// What the shell tells the host once.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// The app's version; "0" in a bundle without one.
    pub version: String,
    /// The viewer's zone, for day groups and derived titles.
    pub zone: FixedOffset,
    /// The OS the host runs on: which permissions are listed, and whether
    /// sentences say "this Mac" or "this computer".
    pub platform: Platform,
    /// A package manager delivers the updates: General offers no check and
    /// `updates.check` checks nothing.
    pub updates_managed: bool,
    /// The shell runs from an installed app bundle (not a disk image, not
    /// translocated). On the Mac only such a launch registers the login
    /// item at first launch ([`Host::register_login_item_on_first_launch`]).
    /// Not read off the Mac.
    pub installed_bundle: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
            version: "0".to_owned(),
            zone: crate::labels::utc(),
            platform: Platform::CURRENT,
            updates_managed: false,
            installed_bundle: false,
        }
    }
}

/// The destructive confirmation: a native alert in the shell, a stub in
/// tests.
pub type Confirm = Box<dyn Fn(&ConfirmDestructiveParams) -> bool + Send + Sync>;

/// The folder chooser: takes the folder to start in and returns the
/// choice, `None` when cancelled.
pub type ChooseFolder = Box<dyn Fn(Option<&Path>) -> Option<PathBuf> + Send + Sync>;

/// The shell's tray ([`Host::on_tray`]); called while the host publishes,
/// so it must not call into the host.
pub type TraySink = Box<dyn Fn(TrayMeetings) + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The thread that feeds the `recording` topic did not start.
    #[error("the host's recording feed could not start: {0}")]
    FeedThread(std::io::Error),
}

/// Waits on an async boundary on the host's shared current-thread runtime.
/// Called from inside a runtime, the future is awaited on a scoped helper
/// thread instead of panicking. Build timers inside the future: one made on
/// the caller's runtime is not driven here.
pub(crate) fn block_on<F>(future: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let runtime = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("the host's current-thread runtime builds")
    });
    let run = || runtime.block_on(future);
    if tokio::runtime::Handle::try_current().is_err() {
        run()
    } else {
        thread::scope(|scope| scope.spawn(run).join())
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }
}

struct Inner {
    /// The host's own-reader handle ([`Store::with_own_reader`]), so a read
    /// under this lock never waits for a write. Only the lock holder reads
    /// on it.
    store: Arc<Store>,
    app: AppState,
    /// Whether the list had meetings at its last publish, so `app`
    /// republishes only when the list empties or fills.
    has_meetings: bool,
    /// `meetings.select` for a meeting the list does not have yet.
    pending_selection: Option<Uuid>,
    list: MeetingListViewModel,
    detail: Option<MeetingDetailViewModel>,
    progress: ProcessingProgressModel,
    /// Meetings flagged with unconfirmed speakers this launch, pruned at
    /// each reload.
    reviews: BTreeSet<Uuid>,
    /// The clip playing at the last pass, so the detail republishes once it
    /// ends.
    playing_clip: Option<PathBuf>,
    general: GeneralSettingsViewModel,
    audio: AudioSettingsViewModel,
    speech: SpeechSettingsViewModel,
    llm: LlmSettingsViewModel,
    obsidian: ObsidianSettingsViewModel,
    phones: PhonesSettingsViewModel,
    subtitles: BTreeMap<SettingsSection, String>,
    onboarding: OnboardingViewModel,
    publisher: TopicPublisher,
    /// The change mark the rows were last loaded at; `None` reloads at the
    /// next pass.
    reloaded_at: Option<ChangeMark>,
}

/// What every clone of a [`Host`] shares.
struct Shared {
    store: Arc<Store>,
    services: Services,
    config: HostConfig,
    inner: Mutex<Inner>,
    sink: Mutex<Option<Arc<dyn EventSink>>>,
    /// Held for a whole publish, so snapshots reach the sink in build
    /// order while `inner` is free in between.
    publishing: Mutex<()>,
    dialogs: RwLock<Dialogs>,
    /// The `recording` topic's feed, which takes none of the locks above.
    feed: RecordingFeed,
    /// The shell's tray, told the meeting rows with every publish that
    /// moved them.
    tray: Mutex<Option<TraySink>>,
    /// What asks the reload owner for a pass ([`Host::request_reload`]).
    reloads: Reloads,
}

/// The reload owner's inbox: a forced pass is owed while `forced` is set.
/// Takes no host lock, so anything may ask.
#[derive(Default)]
struct Reloads {
    forced: AtomicBool,
    wake: tokio::sync::Notify,
    /// Every request so far, for the tests.
    requests: AtomicU64,
}

impl Shared {
    fn request_reload(&self) {
        self.reloads.requests.fetch_add(1, Ordering::Relaxed);
        self.reloads.forced.store(true, Ordering::SeqCst);
        self.reloads.wake.notify_one();
    }
}

impl recording_feed::Source for Shared {
    fn feed(&self) -> &RecordingFeed {
        &self.feed
    }

    fn status(&self) -> RecorderStatus {
        self.services.recorder.status()
    }

    fn sink(&self) -> Option<Arc<dyn EventSink>> {
        lock(&self.sink).clone()
    }

    fn moved(&self) {
        self.request_reload();
    }
}

struct Dialogs {
    confirm: Confirm,
    choose_folder: ChooseFolder,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // The feed's thread holds a `Weak`; woken, it finds no host and ends.
        self.feed.wake();
    }
}

/// The bridge host for all three windows. Cheap to clone: every clone is a
/// handle on the same view models, bound to the window it answers for
/// ([`Host::for_window`]).
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::sync::Arc;
/// use steno_bridge::{BridgeEvent, BridgeHost, BridgeTopic, BridgeWindow, EventSink};
/// use steno_core::Store;
/// use steno_host::fakes::FakeServices;
/// use steno_host::{Host, HostConfig};
///
/// #[derive(Default)]
/// struct Sink(std::sync::Mutex<Vec<BridgeTopic>>);
/// impl EventSink for Sink {
///     fn emit(&self, event: BridgeEvent) {
///         self.0.lock().unwrap().push(event.topic);
///     }
/// }
///
/// let now = steno_core::json::parse_date("2026-09-29T12:50:00.000Z").unwrap();
/// let store = Arc::new(Store::in_memory()?);
/// let fakes = FakeServices::new(now);
/// let host = Host::new(store.clone(), fakes.services(), HostConfig::default())?
///     .with_dialogs(Box::new(|_| true), Box::new(|_| None));
/// let sink = Arc::new(Sink::default());
/// host.attach(sink.clone());
///
/// // `page.ready` publishes every topic once; `recording` comes from the feed.
/// host.page_ready()?;
/// let published = || sink.0.lock().unwrap().len();
/// let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
/// while published() < 12 && std::time::Instant::now() < deadline {
///     std::thread::sleep(std::time::Duration::from_millis(1));
/// }
/// assert_eq!(published(), 12);
///
/// // A write elsewhere moves the change mark; the next pass republishes.
/// let mut settings = store.settings()?;
/// settings.meeting_detection_enabled = false;
/// store.save_settings(&settings)?;
/// host.reload_pass();
/// assert_eq!(
///     host.snapshot(BridgeTopic::SettingsGeneral).unwrap()["detectionEnabled"],
///     false
/// );
///
/// let onboarding = steno_bridge::Dispatcher::new(host.for_window(BridgeWindow::Onboarding));
/// assert_eq!(
///     onboarding.dispatch_json(r#"{"id":"1","method":"onboarding.back","params":null}"#),
///     r#"{"id":"1"}"#
/// );
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Host {
    shared: Arc<Shared>,
    window: BridgeWindow,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("window", &self.window)
            .field("config", &self.shared.config)
            .finish_non_exhaustive()
    }
}

/// Every topic, in the order `page.ready` publishes them. `recording`
/// comes from its feed instead.
pub const TOPICS: [BridgeTopic; 12] = [
    BridgeTopic::App,
    BridgeTopic::Recording,
    BridgeTopic::Progress,
    BridgeTopic::MeetingsList,
    BridgeTopic::MeetingDetail,
    BridgeTopic::SettingsGeneral,
    BridgeTopic::SettingsRecording,
    BridgeTopic::SettingsTranscription,
    BridgeTopic::SettingsSummaries,
    BridgeTopic::SettingsExport,
    BridgeTopic::SettingsPhone,
    BridgeTopic::Onboarding,
];

/// What a model download or remove changes.
const MODEL_TOPICS: [BridgeTopic; 2] =
    [BridgeTopic::SettingsTranscription, BridgeTopic::Onboarding];

const SECTION_TOPICS: [BridgeTopic; 6] = [
    BridgeTopic::SettingsGeneral,
    BridgeTopic::SettingsRecording,
    BridgeTopic::SettingsTranscription,
    BridgeTopic::SettingsSummaries,
    BridgeTopic::SettingsExport,
    BridgeTopic::SettingsPhone,
];

/// What a delete of a meeting that may still be recorded says.
const STILL_RECORDING: &str = "This meeting is still recording.";

/// A confirmed delete refused because the recorder's recovery state for
/// the meeting could not be forgotten.
const COULD_NOT_DELETE: &str = "Steno could not delete this meeting.";

pub const DELETE_MEETING_MESSAGE: &str = "The transcript, summary, tasks and the recording on this Mac are removed. Files already exported to Obsidian stay. People stay.";

/// [`DELETE_MEETING_MESSAGE`] on Windows and Linux.
pub const DELETE_MEETING_MESSAGE_ELSEWHERE: &str = "The transcript, summary, tasks and the recording on this computer are removed. Files already exported to Obsidian stay. People stay.";

pub fn delete_recording_prompt() -> ConfirmDestructiveParams {
    ConfirmDestructiveParams {
        title: "Delete this recording now?".to_owned(),
        message: "The recording is deleted shortly. The transcript, summary and exports stay."
            .to_owned(),
        confirm_title: "Delete recording".to_owned(),
    }
}

/// What "Process again" asks first when the meeting has a confirmed or
/// named speaker: a name the new run cannot match again is cleared, and
/// the note is delivered again.
pub fn process_again_prompt() -> ConfirmDestructiveParams {
    ConfirmDestructiveParams {
        title: "Process this meeting again?".to_owned(),
        message: "Speaker names Steno cannot find again are cleared, and the note is written \
                  again. If you edited it, your version stays and Steno's goes beside it."
            .to_owned(),
        confirm_title: "Process again".to_owned(),
    }
}

/// The warning before Pair again, in the words of `platform`.
#[must_use]
pub fn pair_again_prompt(platform: Platform) -> ConfirmDestructiveParams {
    ConfirmDestructiveParams {
        title: "Pair every phone again?".to_owned(),
        message: platform
            .mac_or(
                "Steno makes a new pairing for this Mac and removes the phones paired with it. Each phone must scan a new code before it can send recordings again.",
                "Steno makes a new pairing for this computer and removes the phones paired with it. Each phone must scan a new code before it can send recordings again.",
            )
            .to_owned(),
        confirm_title: "Pair again".to_owned(),
    }
}

/// System Settings > Privacy & Security > Files and Folders, opened when
/// the write to the vault was refused.
pub const FILES_AND_FOLDERS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders";

/// The preferences flag that counts the first launch on Linux and Windows
/// (and, before, the Mac).
pub const LOGIN_ITEM_REGISTERED_KEY: &str = "steno.loginItemRegistered";

/// The flag that counts the first launch on the Mac, where the login item
/// is `SMAppService.mainApp`. Its own key: an earlier build set
/// [`LOGIN_ITEM_REGISTERED_KEY`] for a Launch Agent that is now removed.
pub const MAIN_APP_REGISTERED_KEY: &str = "steno.mainAppRegistered";

/// The flag that counts the first launch on `platform`
/// ([`Host::register_login_item_on_first_launch`]).
#[must_use]
pub fn first_launch_key(platform: Platform) -> &'static str {
    match platform {
        Platform::Macos => MAIN_APP_REGISTERED_KEY,
        Platform::Windows | Platform::Linux => LOGIN_ITEM_REGISTERED_KEY,
    }
}

fn no_such_meeting() -> BridgeError {
    BridgeError::not_found("No meeting with that id is listed.")
}

fn no_selection() -> BridgeError {
    BridgeError::not_found("No meeting is selected.")
}

impl Host {
    /// Loads every view model; nothing is published until a sink is
    /// attached and `page.ready` arrives. Holds a reader of its own until
    /// the last clone drops, so a second host over the same store fails
    /// with [`StoreError::OwnReaderOpen`].
    pub fn new(
        store: Arc<Store>,
        services: Services,
        config: HostConfig,
    ) -> Result<Self, HostError> {
        let own = Arc::new(store.with_own_reader()?);
        // Before the loads, so a change after them reloads at the next pass.
        let reloaded_at = own.change_mark().ok();
        let recorder = services.recorder.status();
        let key = KeyRead::from(block_on(services.secrets.secret(&SecretKey::llm_api_key())));
        let mut general = GeneralSettingsViewModel::new(&services);
        general.load(&own, &services);
        let mut audio = AudioSettingsViewModel::new();
        audio.load(&own, &services);
        let mut speech = SpeechSettingsViewModel::new();
        speech.load(&own, &services);
        let mut llm = LlmSettingsViewModel::new();
        llm.load(&own, &services, key.clone());
        let mut obsidian = ObsidianSettingsViewModel::default();
        obsidian.load(&own);
        let mut phones = PhonesSettingsViewModel::new(&services);
        phones.load(&services);
        phones.refresh(&services);
        let subtitles = overview::refresh(&own, &services, config.platform, &config.version);
        let mut onboarding = OnboardingViewModel::new(config.platform);
        onboarding.load(&own, &services, key);
        let mut list = MeetingListViewModel::new(config.zone);
        list.reload(&own);
        let mut progress = ProcessingProgressModel {
            speech_fallback: services.speech_models.speech_fallback(),
            ..ProcessingProgressModel::default()
        };
        progress.meetings_changed(&list.all, services.clock.now());
        let app = AppState {
            speech_engine_notice: services.preferences.flag(engine_notice::PENDING_KEY),
            stored_settings: own.settings().ok(),
            phone: Self::phone_card(&services),
            ..AppState::default()
        };
        let publisher = TopicPublisher::new(
            TOPICS
                .into_iter()
                .filter(|topic| *topic != BridgeTopic::Recording)
                .collect(),
        );
        let shared = Arc::new(Shared {
            store,
            services,
            config,
            inner: Mutex::new(Inner {
                store: own,
                app,
                has_meetings: false,
                pending_selection: None,
                list,
                detail: None,
                progress,
                reviews: BTreeSet::new(),
                playing_clip: None,
                general,
                audio,
                speech,
                llm,
                obsidian,
                phones,
                subtitles,
                onboarding,
                publisher,
                reloaded_at,
            }),
            sink: Mutex::new(None),
            publishing: Mutex::new(()),
            dialogs: RwLock::new(Dialogs {
                confirm: Box::new(|_| false),
                choose_folder: Box::new(|_| None),
            }),
            feed: RecordingFeed::new(),
            tray: Mutex::new(None),
            reloads: Reloads::default(),
        });
        RecordingFeed::start(&shared, (recorder.state, recorder.meeting_id))
            .map_err(HostError::FeedThread)?;
        Ok(Host {
            shared,
            window: BridgeWindow::Main,
        })
    }

    /// The shell's two native surfaces. Without them every destructive
    /// prompt declines and every chooser cancels.
    #[must_use]
    pub fn with_dialogs(self, confirm: Confirm, choose_folder: ChooseFolder) -> Self {
        *self
            .shared
            .dialogs
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Dialogs {
            confirm,
            choose_folder,
        };
        self
    }

    fn dialogs(&self) -> RwLockReadGuard<'_, Dialogs> {
        self.shared
            .dialogs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn confirm(&self, prompt: &ConfirmDestructiveParams) -> bool {
        (self.dialogs().confirm)(prompt)
    }

    /// The same host, answering for `window`.
    #[must_use]
    pub fn for_window(&self, window: BridgeWindow) -> Host {
        Host {
            shared: self.shared.clone(),
            window,
        }
    }

    #[must_use]
    pub fn window(&self) -> BridgeWindow {
        self.window
    }

    /// The sink snapshots go to.
    pub fn attach(&self, sink: Arc<dyn EventSink>) {
        *lock(&self.shared.sink) = Some(sink);
    }

    /// Where the tray's meeting rows go: now, and with every publish of
    /// `progress` or `meetings.list`.
    pub fn on_tray(&self, sink: TraySink) {
        let _publishing = lock(&self.shared.publishing);
        sink(self.tray_meetings());
        *lock(&self.shared.tray) = Some(sink);
    }

    #[must_use]
    pub fn tray_meetings(&self) -> TrayMeetings {
        let inner = self.lock();
        tray::tray_meetings(&inner.list, &inner.progress, &inner.reviews)
    }

    #[must_use]
    pub fn services(&self) -> &Services {
        &self.shared.services
    }

    /// The shared store, for code that runs without the host's lock.
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.shared.store
    }

    /// The iPhone card for the `app` topic: the first paired device.
    fn phone_card(services: &Services) -> Option<AppPhone> {
        let handover = services.handover.as_ref()?;
        let device = handover.paired_devices().ok()?.into_iter().next()?;
        Some(AppPhone {
            name: device.name,
            last_sync_at: device.last_seen_at,
            is_reachable: matches!(
                handover.state(),
                crate::services::ListenerState::Listening(_)
            ),
        })
    }

    fn now(&self) -> DateTime<Utc> {
        self.shared.services.clock.now()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        lock(&self.shared.inner)
    }

    /// The topic's snapshot as the view models stand, after the publish
    /// hooks (the list's fill, the detail swap), which may change state and
    /// queue topics for the next publish. `None` for a topic the host does
    /// not send; `null` for `meeting.detail` without a selection.
    pub fn snapshot(&self, topic: BridgeTopic) -> Option<Value> {
        if topic == BridgeTopic::Recording {
            let status = self.shared.services.recorder.status();
            return to_value(main_snapshots::recording_snapshot(&status)).ok();
        }
        let mut inner = self.lock();
        self.build(&mut inner, topic)
    }

    /// A test's hold on the view models.
    #[cfg(test)]
    pub(crate) fn hold_view_models(&self) -> impl Sized + '_ {
        self.lock()
    }

    /// The `recording` feed's thread, taken, so a test can tell it ended.
    #[cfg(test)]
    pub(crate) fn take_feed_thread(&self) -> Option<thread::JoinHandle<()>> {
        self.shared.feed.take_thread()
    }

    /// Every topic but `recording`.
    fn build(&self, inner: &mut Inner, topic: BridgeTopic) -> Option<Value> {
        let now = self.now();
        let snapshot = match topic {
            BridgeTopic::App => to_value(main_snapshots::app_snapshot(
                &inner.app,
                inner.has_meetings,
                &self.shared.config.version,
                self.shared.config.platform,
            )),
            BridgeTopic::Recording => return None,
            BridgeTopic::Progress => to_value(main_snapshots::progress_snapshot(&inner.progress)),
            BridgeTopic::MeetingsList => {
                Self::follow_list_fill(inner);
                self.sync_detail(inner);
                to_value(main_snapshots::list_snapshot(&inner.list, now))
            }
            BridgeTopic::MeetingDetail => {
                self.sync_detail(inner);
                self.follow_key_withheld(inner);
                let playing = inner
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.speakers.playing(&*self.shared.services.clip_player));
                inner
                    .detail
                    .as_ref()
                    .and_then(|detail| {
                        main_snapshots::detail_snapshot(
                            detail,
                            playing,
                            now,
                            self.shared.config.zone,
                        )
                    })
                    .map_or(Ok(Value::Null), to_value)
            }
            BridgeTopic::SettingsGeneral => to_value(settings_snapshots::general(
                &inner.general,
                &self.shared.services,
                inner.subtitle(SettingsSection::General),
                &self.shared.config.version,
                self.shared.config.updates_managed,
            )),
            BridgeTopic::SettingsRecording => to_value(settings_snapshots::recording(
                &inner.audio,
                inner.subtitle(SettingsSection::Recording),
                self.shared.config.platform,
            )),
            BridgeTopic::SettingsTranscription => to_value(settings_snapshots::transcription(
                &inner.speech,
                self.shared.services.speech_models.as_ref(),
                inner.subtitle(SettingsSection::Transcription),
            )),
            BridgeTopic::SettingsSummaries => to_value(settings_snapshots::summaries(
                &inner.llm,
                inner.subtitle(SettingsSection::Summaries),
                self.shared.config.platform,
            )),
            BridgeTopic::SettingsExport => to_value(settings_snapshots::export(
                &inner.obsidian,
                inner.subtitle(SettingsSection::Export),
            )),
            BridgeTopic::SettingsPhone => to_value(settings_snapshots::phone(
                &inner.phones,
                &self.shared.services,
                inner.subtitle(SettingsSection::Phone),
                self.shared.config.platform,
            )),
            BridgeTopic::Onboarding => to_value(onboarding::snapshot(
                &inner.onboarding,
                &inner.speech,
                self.shared.services.speech_models.as_ref(),
            )),
        };
        snapshot.ok()
    }

    /// Publishes every pending topic: builds under the lock, emits once it
    /// is released and the page is ready, and runs the side effects. Never
    /// called with the view models held.
    fn publish(&self) {
        let _publishing = lock(&self.shared.publishing);
        // Bounded: each pass only schedules what a consumed request changed.
        for _ in 0..4 {
            let (sink, batch, meetings) = {
                let mut inner = self.lock();
                let due = inner.publisher.take_due();
                if due.is_empty() {
                    break;
                }
                let meetings = due
                    .iter()
                    .any(|topic| matches!(topic, BridgeTopic::Progress | BridgeTopic::MeetingsList))
                    .then(|| tray::tray_meetings(&inner.list, &inner.progress, &inner.reviews));
                // Nothing goes out before `page.ready`; snapshots are still
                // built for their side effects.
                let sink = lock(&self.shared.sink)
                    .clone()
                    .filter(|_| inner.publisher.is_page_ready());
                let mut batch = Vec::with_capacity(due.len());
                for topic in due {
                    let snapshot = self.build(&mut inner, topic).filter(|_| sink.is_some());
                    if topic == BridgeTopic::App {
                        Self::did_publish_app(&mut inner, snapshot.is_some());
                    }
                    batch.extend(snapshot.map(|payload| BridgeEvent::new(topic, payload)));
                }
                (sink, batch, meetings)
            };
            if let Some(sink) = sink {
                for event in batch {
                    sink.emit(event);
                }
            }
            if let (Some(meetings), Some(tray)) = (meetings, lock(&self.shared.tray).as_ref()) {
                tray(meetings);
            }
        }
    }

    /// Consumes the controller's requests once the `app` snapshot carried
    /// them. A Settings deep link is cleared at the first emit to either
    /// window; the shell also passes the section in the route of a window
    /// it opens, so a later window still lands on it.
    fn did_publish_app(inner: &mut Inner, emitted: bool) {
        if let Some(requested) = inner.app.requested_meeting_id.take() {
            inner.pending_selection = None;
            inner.list.selection = Some(requested);
            inner.publisher.schedule(BridgeTopic::App);
            inner.publisher.schedule(BridgeTopic::MeetingsList);
            inner.publisher.schedule(BridgeTopic::MeetingDetail);
        }
        if emitted && inner.app.requested_settings_section.take().is_some() {
            inner.publisher.schedule(BridgeTopic::App);
        }
    }

    /// The list publish's side effects: a pending selection lands once its
    /// row is listed, the first fill selects the newest meeting, and
    /// `has_meetings` flips only when the list empties or fills.
    fn follow_list_fill(inner: &mut Inner) {
        let filled = !inner.list.all.is_empty();
        if let Some(pending) = inner.pending_selection
            && inner.list.contains(pending)
        {
            inner.pending_selection = None;
            inner.list.selection = Some(pending);
        }
        if filled && !inner.has_meetings && inner.list.selection.is_none() {
            inner.list.selection = inner.list.meetings.first().map(|meeting| meeting.id);
        }
        if filled != inner.has_meetings {
            inner.has_meetings = filled;
            inner.publisher.schedule(BridgeTopic::App);
        }
    }

    /// One detail model per selected meeting.
    fn sync_detail(&self, inner: &mut Inner) {
        let selection = inner.list.selection;
        if selection == inner.detail.as_ref().map(|detail| detail.id) {
            return;
        }
        if let Some(mut leaving) = inner.detail.take() {
            leaving.view_disappeared(
                &*self.shared.services.pipeline,
                &*self.shared.services.clip_player,
            );
        }
        if let Some(selection) = selection {
            let mut detail =
                MeetingDetailViewModel::new(selection, inner.app.stored_settings.as_ref());
            detail.reload(
                &inner.store,
                &*self.shared.services.file_system,
                &*self.shared.services.pipeline,
            );
            inner.detail = Some(detail);
        }
        inner.publisher.schedule(BridgeTopic::MeetingDetail);
    }

    /// A forced pass ([`Self::reload_pass`]) on the caller's thread,
    /// whatever the change mark says. For the launch and the tests;
    /// everything else calls [`Self::request_reload`].
    pub fn store_changed(&self) {
        self.pass(true);
    }

    /// Asks the reload owner for a forced pass. Takes no host lock, so any
    /// thread may ask with any lock held; requests during a pass fold into
    /// one more pass.
    pub fn request_reload(&self) {
        self.shared.request_reload();
    }

    /// Completes once a reload was asked for since the last wait.
    pub async fn reload_requested(&self) {
        self.shared.reloads.wake.notified().await;
    }

    /// How many reloads were asked for, for the tests.
    #[doc(hidden)]
    #[must_use]
    pub fn reload_request_count(&self) -> u64 {
        self.shared.reloads.requests.load(Ordering::Relaxed)
    }

    /// The host's own store handle, so a test can hold its reader. Takes
    /// the view-model lock; tests only.
    #[doc(hidden)]
    #[must_use]
    pub fn own_store(&self) -> Arc<Store> {
        self.lock().store.clone()
    }

    /// One pass of the reload owner. Runs the light part (pairing,
    /// subtitles, the withheld key, the system audio rows) every time, and
    /// reloads rows and stored settings only when a reload was asked for or
    /// the change mark moved. The flag is taken before the mark is read and
    /// the mark before the reload reads, so a request or commit during a
    /// pass is caught by the next. Changed settings reload the Settings
    /// sections too, replacing any unsaved draft.
    pub fn reload_pass(&self) {
        self.pass(false);
    }

    fn pass(&self, force: bool) {
        let forced = self.shared.reloads.forced.swap(false, Ordering::SeqCst) || force;
        {
            let mut guard = self.lock();
            let inner = &mut *guard;
            let mark = inner.store.change_mark().ok();
            if forced || mark.is_none() || mark != inner.reloaded_at {
                inner.reloaded_at = mark;
                self.reload_rows(inner);
                self.follow_stored_settings(inner, true);
            }
            self.follow_outside_the_store(inner);
        }
        self.publish();
    }

    /// The list, the progress over it and the open detail, all scheduled.
    fn reload_rows(&self, inner: &mut Inner) {
        let now = self.now();
        inner.list.reload(&inner.store);
        inner.progress.meetings_changed(&inner.list.all, now);
        tray::prune_reviews(&mut inner.reviews, &inner.list);
        if let Some(detail) = inner.detail.as_mut() {
            detail.reload(
                &inner.store,
                &*self.shared.services.file_system,
                &*self.shared.services.pipeline,
            );
        }
        for topic in [
            BridgeTopic::Progress,
            BridgeTopic::MeetingsList,
            BridgeTopic::MeetingDetail,
        ] {
            inner.publisher.schedule(topic);
        }
    }

    /// A pass's light part: the pairing poll (a phone that arrived or a
    /// code that ran out closes the code), the system audio rows once the
    /// recorder is idle, and the detail once a clip ended.
    fn follow_outside_the_store(&self, inner: &mut Inner) {
        let now = self.now();
        inner
            .phones
            .refresh_after_pairing(&self.shared.services, now);
        inner.publisher.schedule(BridgeTopic::SettingsPhone);
        let playing = self.shared.services.clip_player.playing();
        if playing != inner.playing_clip {
            inner.playing_clip = playing;
            inner.publisher.schedule(BridgeTopic::MeetingDetail);
        }
        if self.shared.services.recorder.status().state == RecordingState::Idle {
            if inner.audio.waits_for_recording.take().is_some() {
                inner.publisher.schedule(BridgeTopic::SettingsRecording);
            }
            if inner.onboarding.waits_for_recording.take().is_some() {
                inner.publisher.schedule(BridgeTopic::Onboarding);
            }
        }
        self.follow_key_and_subtitles(inner);
    }

    /// Pokes the `recording` feed and returns with no lock taken, so the
    /// recorder's threads never wait for the host.
    pub fn recorder_changed(&self) {
        self.shared.feed.poke();
    }

    /// One event from the pipeline's bus.
    pub fn apply_meeting_event(&self, event: &MeetingEvent) {
        {
            let mut inner = self.lock();
            let now = self.now();
            inner.progress.apply(event, now);
            inner.publisher.schedule(BridgeTopic::Progress);
            if let MeetingEvent::SpeakersNeedReview { meeting_id, .. } = event {
                inner.reviews.insert(*meeting_id);
            }
            if let MeetingEvent::OperationFailed {
                meeting_id,
                operation,
                failure,
                ..
            } = event
                && let Some(detail) = inner.detail.as_mut()
                && detail.id == *meeting_id
            {
                detail.operation_failed(*operation, failure);
                inner.publisher.schedule(BridgeTopic::MeetingDetail);
            }
        }
        self.publish();
    }

    pub fn phones_changed(&self) {
        {
            let mut inner = self.lock();
            inner.phones.refresh(&self.shared.services);
            inner.publisher.schedule(BridgeTopic::SettingsPhone);
            self.refresh_subtitles(&mut inner);
        }
        self.publish();
    }

    /// An update check started or ended outside a command: General and
    /// the overview follow.
    pub fn updates_changed(&self) {
        {
            let mut inner = self.lock();
            inner.publisher.schedule(BridgeTopic::SettingsGeneral);
            self.refresh_subtitles(&mut inner);
        }
        self.publish();
    }

    /// The menu bar or the detection prompt asked for a meeting: it rides
    /// on the next `app` snapshot and becomes the selection.
    pub fn request_meeting(&self, meeting_id: Uuid) {
        {
            let mut inner = self.lock();
            inner.app.requested_meeting_id = Some(meeting_id);
            inner.publisher.schedule(BridgeTopic::App);
        }
        self.publish();
    }

    /// Deep link into Settings.
    pub fn open_settings(&self, section: SettingsSection) {
        {
            let mut inner = self.lock();
            inner.app.requested_settings_section = Some(section);
            inner.publisher.schedule(BridgeTopic::App);
        }
        self.publish();
    }

    #[must_use]
    pub fn should_open_onboarding(&self) -> bool {
        OnboardingViewModel::should_open(
            &self.shared.store,
            &self.shared.services,
            self.shared.config.platform,
        )
    }

    /// Registers the login item on the first launch with the setting on,
    /// once, and only while it reads `NotRegistered`; afterwards only the
    /// General switch changes it. The launch is not counted
    /// ([`first_launch_key`]) when the system manages the item, when the
    /// registration failed, or on the Mac when the item reads `NotFound`
    /// (a run outside a bundle, such as `cargo run`, would otherwise stop
    /// the installed app's first registration). On the Mac a launch from
    /// outside an installed bundle does nothing
    /// ([`HostConfig::installed_bundle`]).
    pub fn register_login_item_on_first_launch(&self) {
        use crate::services::LoginItemStatus;
        let config = &self.shared.config;
        if config.platform == Platform::Macos && !config.installed_bundle {
            return;
        }
        let preferences = &self.shared.services.preferences;
        let login_item = &self.shared.services.login_item;
        let key = first_launch_key(config.platform);
        if preferences.flag(key) || login_item.status() == LoginItemStatus::Managed {
            return;
        }
        let Ok(settings) = self.shared.store.settings() else {
            return;
        };
        if !settings.launch_at_login {
            return;
        }
        let counted = match login_item.status() {
            LoginItemStatus::NotRegistered => match login_item.set_enabled(true) {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(%error, "the first launch could not register the login item");
                    false
                }
            },
            LoginItemStatus::NotFound => config.platform != Platform::Macos,
            LoginItemStatus::Enabled | LoginItemStatus::RequiresApproval => true,
            LoginItemStatus::Managed => false,
        };
        if counted {
            preferences.set_flag(key, true);
        }
        let mut inner = self.lock();
        inner.general.login_item = login_item.status();
    }

    /// The onboarding window closed. Closing counts as having seen the
    /// pages, so it reopens only for a missing required permission, and
    /// the model starts over at page 1.
    pub fn onboarding_window_closed(&self) {
        OnboardingViewModel::mark_completed(&self.shared.services);
        // Closing over the import step skips it until the next launch,
        // without waiting for a prompt that is up.
        if let Some(import) = &self.shared.services.swift_import
            && import.status().stage == crate::services::SwiftImportStage::Pending
        {
            import.skip();
        }
        let key = self.read_key();
        {
            let mut inner = self.lock();
            let mut fresh = OnboardingViewModel::new(self.shared.config.platform);
            fresh.load(&inner.store, &self.shared.services, key);
            inner.onboarding = fresh;
            inner.publisher.schedule(BridgeTopic::Onboarding);
        }
        self.publish();
    }

    /// The secret store answers again after a read failed while the
    /// keyring asked: Summaries and onboarding read the key again, unless
    /// it holds an unsaved edit ([`LlmSettingsViewModel::reload_key`]).
    pub fn secrets_changed(&self) {
        let read_at = {
            let inner = self.lock();
            (inner.llm.key_version(), inner.onboarding.llm.key_version())
        };
        let key = self.read_key();
        {
            let mut inner = self.lock();
            let services = &self.shared.services;
            inner.llm.reload_key(services, key.clone(), read_at.0);
            inner.onboarding.llm.reload_key(services, key, read_at.1);
            inner.publisher.schedule(BridgeTopic::SettingsSummaries);
            inner.publisher.schedule(BridgeTopic::Onboarding);
        }
        self.publish();
    }

    /// The stored API key; never asks the user.
    fn read_key(&self) -> KeyRead {
        KeyRead::from(block_on(
            self.shared
                .services
                .secrets
                .secret(&SecretKey::llm_api_key()),
        ))
    }

    fn with_detail<R>(
        inner: &mut Inner,
        body: impl FnOnce(&mut MeetingDetailViewModel, &Store) -> R,
    ) -> Outcome<R> {
        let store = &inner.store;
        inner
            .detail
            .as_mut()
            .map(|detail| body(detail, store))
            .ok_or_else(no_selection)
    }

    /// A command on the selected detail (`no_selection` without one) that
    /// changes only the detail's own state.
    fn detail_command<R>(&self, body: impl FnOnce(&mut MeetingDetailViewModel) -> R) -> Outcome<R> {
        self.on_detail(
            |detail, _| body(detail),
            |inner| {
                inner.publisher.schedule(BridgeTopic::MeetingDetail);
            },
        )
    }

    /// A command on the selected detail after which the store changed: the
    /// detail and the list reload before publishing.
    fn detail_write<R>(
        &self,
        body: impl FnOnce(&mut MeetingDetailViewModel, &Store) -> R,
    ) -> Outcome<R> {
        self.on_detail(body, |inner| self.reload_detail(inner))
    }

    fn on_detail<R>(
        &self,
        body: impl FnOnce(&mut MeetingDetailViewModel, &Store) -> R,
        after: impl FnOnce(&mut Inner),
    ) -> Outcome<R> {
        let outcome = {
            let mut inner = self.lock();
            let outcome = Self::with_detail(&mut inner, body);
            after(&mut inner);
            outcome
        };
        self.publish();
        outcome
    }

    /// Runs the folder chooser from `current` and `apply` on the choice.
    fn choose(&self, current: Option<&Path>, apply: impl FnOnce(&Path)) -> ChosenPathReply {
        let chosen = (self.dialogs().choose_folder)(current);
        if let Some(folder) = &chosen {
            apply(folder);
        }
        ChosenPathReply {
            path: chosen.map(|path| path.to_string_lossy().into_owned()),
        }
    }

    fn reload_detail(&self, inner: &mut Inner) {
        if let Some(detail) = inner.detail.as_mut() {
            detail.reload(
                &inner.store,
                &*self.shared.services.file_system,
                &*self.shared.services.pipeline,
            );
        }
        inner.list.reload(&inner.store);
        inner.publisher.schedule(BridgeTopic::MeetingsList);
        inner.publisher.schedule(BridgeTopic::MeetingDetail);
    }

    /// Refreshes the sidebar subtitles; a change republishes every section.
    fn refresh_subtitles(&self, inner: &mut Inner) {
        let next = overview::refresh(
            &inner.store,
            &self.shared.services,
            self.shared.config.platform,
            &self.shared.config.version,
        );
        if next != inner.subtitles {
            inner.subtitles = next;
            for topic in SECTION_TOPICS {
                inner.publisher.schedule(topic);
            }
        }
    }

    /// After a command that may have written settings: re-reads them, and
    /// when `reload_sections` and they changed, reloads the Settings
    /// sections. The subtitles and the speech fallback follow either way.
    fn follow_settings(&self, inner: &mut Inner, reload_sections: bool) {
        self.follow_stored_settings(inner, reload_sections);
        self.follow_speech_fallback(inner);
        self.follow_key_and_subtitles(inner);
    }

    /// A Summaries save that rebuilt the pipeline ends a pending "Use
    /// again" rebuild, and a meeting waiting for its models says the
    /// Neural Engine is off exactly while Transcription does.
    fn follow_speech_fallback(&self, inner: &mut Inner) {
        let settings = inner.llm.take_pipeline_rebuilt();
        let onboarding = inner.onboarding.llm.take_pipeline_rebuilt();
        if settings || onboarding {
            let offered = inner.speech.speech_fallback;
            inner.speech.pipeline_rebuilt(&self.shared.services);
            if inner.speech.speech_fallback != offered {
                for topic in MODEL_TOPICS {
                    inner.publisher.schedule(topic);
                }
            }
        }
        if inner.progress.speech_fallback != inner.speech.speech_fallback {
            inner.progress.speech_fallback = inner.speech.speech_fallback;
            inner.publisher.schedule(BridgeTopic::Progress);
        }
    }

    fn follow_stored_settings(&self, inner: &mut Inner, reload_sections: bool) {
        let latest = inner.store.settings().ok();
        if latest != inner.app.stored_settings {
            inner.app.stored_settings = latest;
            inner.publisher.schedule(BridgeTopic::App);
            if let (Some(detail), Some(settings)) =
                (inner.detail.as_mut(), &inner.app.stored_settings)
            {
                detail.apply_settings(settings);
                inner.publisher.schedule(BridgeTopic::MeetingDetail);
            }
            if reload_sections {
                self.reload_sections(inner);
            }
        }
    }

    fn follow_key_and_subtitles(&self, inner: &mut Inner) {
        // A saved key may release a withheld key without a settings change.
        if self.follow_key_withheld(inner) {
            inner.publisher.schedule(BridgeTopic::MeetingDetail);
        }
        self.refresh_subtitles(inner);
    }

    /// Sets the open detail's `key_withheld` from the stored endpoint now
    /// ([`WithheldApiKey`](crate::services::WithheldApiKey)); true when it
    /// changed.
    fn follow_key_withheld(&self, inner: &mut Inner) -> bool {
        let withheld = self
            .shared
            .services
            .withheld_api_key
            .as_ref()
            .zip(inner.app.stored_settings.as_ref())
            .is_some_and(|(withheld, settings)| withheld.withheld(settings));
        let Some(detail) = inner.detail.as_mut() else {
            return false;
        };
        let changed = detail.key_withheld != withheld;
        detail.key_withheld = withheld;
        changed
    }

    /// Every Settings section's `load`; a download in flight keeps its
    /// state. Reads the API key with the lock held, which runs only when the
    /// stored settings changed. On the Mac the Keychain may show its access
    /// dialog here and every window waits for it. A `SecretStore` must not
    /// call back into the host.
    fn reload_sections(&self, inner: &mut Inner) {
        let store = &inner.store;
        let services = &self.shared.services;
        let key = self.read_key();
        inner.general.load(store, services);
        inner.audio.load(store, services);
        inner.speech.load(store, services);
        inner.llm.load(store, services, key);
        inner.obsidian.load(store);
        for topic in SECTION_TOPICS {
            inner.publisher.schedule(topic);
        }
    }

    fn settings_command(&self, topic: BridgeTopic, body: impl FnOnce(&mut Inner)) {
        {
            let mut inner = self.lock();
            body(&mut inner);
            inner.publisher.schedule(topic);
            self.follow_settings(&mut inner, false);
        }
        self.publish();
    }

    /// An onboarding command: page 2 writes the settings Summaries and
    /// Export show, so those reload when it did. Answers what `body`
    /// answered.
    fn onboarding_command<R>(&self, body: impl FnOnce(&mut Inner) -> R) -> R {
        let (answer, finished) = {
            let mut inner = self.lock();
            let was_finished = inner.onboarding.finished;
            let answer = body(&mut inner);
            inner.publisher.schedule(BridgeTopic::Onboarding);
            self.follow_settings(&mut inner, true);
            (answer, !was_finished && inner.onboarding.finished)
        };
        self.publish();
        self.run_pending_probe(BridgeWindow::Onboarding);
        if finished {
            self.shared
                .services
                .opener
                .close_window(BridgeWindow::Onboarding);
        }
        self.check_vault_if_due();
        answer
    }

    /// Page 1's vault row ([`OnboardingViewModel::begin_vault_check`]):
    /// when the check is due, the probe write runs on its own thread, since
    /// macOS holds it while its Files and Folders prompt is up. Called once
    /// the page is ready and after every onboarding command.
    fn check_vault_if_due(&self) {
        let settings = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            let settings = inner.onboarding.begin_vault_check(&inner.store);
            if settings.is_some() {
                inner.publisher.schedule(BridgeTopic::Onboarding);
            }
            settings
        };
        let Some(settings) = settings else {
            return;
        };
        self.publish();
        let host = self.clone();
        let spawned = thread::Builder::new()
            .name("steno-vault-check".to_owned())
            .spawn(move || {
                let outcome = host.shared.services.export_validator.validate(&settings);
                host.finish_vault_check(outcome);
            });
        if let Err(error) = spawned {
            self.finish_vault_check(Err(VaultRefusal {
                message: format!("The vault could not be checked: {error}"),
                not_writable: false,
            }));
        }
    }

    fn finish_vault_check(&self, outcome: Result<(), VaultRefusal>) {
        self.command(&[BridgeTopic::Onboarding], |inner| {
            inner.onboarding.finish_vault_check(outcome);
        });
    }

    /// A command on the Export section whose `edit` may begin a save: the
    /// vault check ([`VaultSave::validate`]) writes a probe file, so it runs
    /// with the lock released.
    fn export_command(
        &self,
        edit: impl FnOnce(&mut ObsidianSettingsViewModel) -> Option<VaultSave>,
    ) {
        let mut save = None;
        self.settings_command(BridgeTopic::SettingsExport, |inner| {
            save = edit(&mut inner.obsidian);
        });
        let Some(save) = save else {
            return;
        };
        let validated = save.validate(&self.shared.services);
        self.settings_command(BridgeTopic::SettingsExport, |inner| {
            inner.obsidian.finish_save(save, validated, &inner.store);
        });
    }

    /// Page 2's vault save, checked as
    /// [`export_command`](Self::export_command) does.
    fn onboarding_vault_command(&self, begin: impl FnOnce(&mut OnboardingViewModel) -> VaultSave) {
        let save = self.onboarding_command(|inner| begin(&mut inner.onboarding));
        let validated = save.validate(&self.shared.services);
        self.onboarding_command(|inner| {
            inner.onboarding.finish_save_vault(
                save,
                validated,
                &inner.store,
                &self.shared.services,
            );
        });
    }

    fn command(&self, topics: &[BridgeTopic], body: impl FnOnce(&mut Inner)) {
        {
            let mut inner = self.lock();
            body(&mut inner);
            for topic in topics {
                inner.publisher.schedule(*topic);
            }
        }
        self.publish();
    }

    /// Whether a request for `kind` is refused for the recording: the
    /// system audio test plays a tone a starting, running or stopping
    /// recording would pick up.
    fn waits_for_recording(&self, kind: PermissionKind) -> bool {
        kind == PermissionKind::SystemAudio
            && self.shared.services.recorder.status().state != RecordingState::Idle
    }

    /// Requests `kind`, and says whether the request waited for the
    /// recording after all (a start during the test stopped it).
    fn request_unless_recording(&self, kind: PermissionKind) -> (PermissionState, bool) {
        let state = self.shared.services.permissions.request(kind);
        let waited = state == PermissionState::Unknown && self.waits_for_recording(kind);
        (state, waited)
    }

    /// Marks a view model busy and publishes, runs `work` with the lock
    /// released, then applies its result and publishes again. `begin`
    /// returns `None` when there is nothing to run.
    fn outside_lock<W, R>(
        &self,
        topic: BridgeTopic,
        begin: impl FnOnce(&mut Inner) -> Option<W>,
        work: impl FnOnce(W) -> R,
        finish: impl FnOnce(&mut Inner, R),
    ) {
        let work_item = {
            let mut inner = self.lock();
            let work_item = begin(&mut inner);
            inner.publisher.schedule(topic);
            self.refresh_subtitles(&mut inner);
            work_item
        };
        self.publish();
        let Some(work_item) = work_item else {
            return;
        };
        let result = work(work_item);
        {
            let mut inner = self.lock();
            finish(&mut inner, result);
            inner.publisher.schedule(topic);
            self.refresh_subtitles(&mut inner);
        }
        self.publish();
    }

    fn summaries_topic(window: BridgeWindow) -> BridgeTopic {
        if window == BridgeWindow::Onboarding {
            BridgeTopic::Onboarding
        } else {
            BridgeTopic::SettingsSummaries
        }
    }

    /// A command on `window`'s Summaries form, then the probe the model
    /// asked for, outside the lock. A save writes the key under the lock,
    /// and a keyring may ask the user first (up to two minutes on Linux):
    /// every window and the tray wait with it.
    fn summaries_command(
        &self,
        window: BridgeWindow,
        body: impl FnOnce(&mut LlmSettingsViewModel, &Store),
    ) {
        {
            let mut inner = self.lock();
            let store = inner.store.clone();
            body(inner.llm_mut(window), &store);
            inner.publisher.schedule(Self::summaries_topic(window));
            self.follow_settings(&mut inner, window == BridgeWindow::Onboarding);
        }
        self.publish();
        self.run_pending_probe(window);
    }

    /// Runs the probe the window's Summaries model asked for, if any, with
    /// the lock released.
    fn run_pending_probe(&self, window: BridgeWindow) {
        if !self.lock().llm_mut(window).probe_pending() {
            return;
        }
        let now = self.now();
        self.outside_lock(
            Self::summaries_topic(window),
            |inner| inner.llm_mut(window).begin_pending_probe(now),
            |probe: Probe| {
                self.shared
                    .services
                    .llm
                    .probe(&probe.settings, probe.api_key.as_deref())
            },
            |inner, outcome| inner.llm_mut(window).finish_probe(outcome),
        );
    }

    /// The download, on its own thread: every progress report publishes,
    /// then the installed size or the failure.
    fn run_download(&self, asset: ModelAsset) {
        let outcome = self
            .shared
            .services
            .speech_models
            .download(asset, &mut |fraction, phase| {
                self.command(&MODEL_TOPICS, |inner| {
                    inner.speech.download_progress(asset, fraction, phase);
                });
            });
        self.models_command(|inner| {
            inner
                .speech
                .finish_download(asset, outcome, &self.shared.services);
        });
    }

    fn models_command(&self, body: impl FnOnce(&mut Inner)) {
        self.settings_command(BridgeTopic::SettingsTranscription, |inner| {
            body(inner);
            inner.publisher.schedule(BridgeTopic::Onboarding);
        });
    }

    /// Applies the keep flag to `meeting_id`, after the delete-recording
    /// prompt when `confirming`; the reply says whether the user went
    /// ahead. The prompt runs with the lock released, so the answer lands
    /// on the meeting it was asked for even if the selection moved. A
    /// meeting deleted meanwhile is `not_found`.
    fn set_keep_audio(
        &self,
        keep: bool,
        meeting_id: Uuid,
        confirming: bool,
    ) -> Outcome<ConfirmReply> {
        if confirming && !self.confirm(&delete_recording_prompt()) {
            return Ok(ConfirmReply { confirmed: false });
        }
        let outcome = {
            let mut inner = self.lock();
            if inner.store.meeting(meeting_id)?.is_none() {
                return Err(no_such_meeting());
            }
            let applied = detail::set_keep_audio(
                meeting_id,
                keep,
                &inner.store,
                &*self.shared.services.pipeline,
            );
            let outcome = match (applied, inner.detail.as_mut()) {
                (Ok(()), _) => Ok(ConfirmReply { confirmed: true }),
                (Err(error), Some(detail)) if detail.id == meeting_id => {
                    detail.error = Some(error);
                    Ok(ConfirmReply { confirmed: true })
                }
                (Err(error), _) => Err(BridgeError::failed(error)),
            };
            self.reload_detail(&mut inner);
            outcome
        };
        self.publish();
        outcome
    }

    /// Marks the iPhone section busy; `false` while Try again or Pair again
    /// already runs.
    fn begin_phones_busy(&self) -> bool {
        self.change_phones(PhonesSettingsViewModel::begin_busy)
    }

    fn change_phones<T>(&self, change: impl FnOnce(&mut PhonesSettingsViewModel) -> T) -> T {
        let changed = {
            let mut inner = self.lock();
            let changed = change(&mut inner.phones);
            inner.publisher.schedule(BridgeTopic::SettingsPhone);
            changed
        };
        self.publish();
        changed
    }

    /// Try again's or Pair again's outcome, and what it may have changed:
    /// the subtitle, the phone card and the import step.
    fn finish_phones_busy(&self, finish: impl FnOnce(&mut PhonesSettingsViewModel, &Services)) {
        let services = &self.shared.services;
        let import = {
            let mut inner = self.lock();
            finish(&mut inner.phones, services);
            inner.publisher.schedule(BridgeTopic::SettingsPhone);
            self.refresh_subtitles(&mut inner);
            inner.app.phone = Self::phone_card(services);
            inner.publisher.schedule(BridgeTopic::App);
            services
                .swift_import
                .clone()
                .filter(|_| inner.onboarding.import.is_some() && !inner.onboarding.importing)
        };
        self.publish();
        if let Some(import) = import {
            self.finish_swift_import(import.status(), false);
        }
    }

    /// The import's outcome on the step, and the key and listener it may
    /// have changed.
    fn finish_swift_import(&self, status: crate::services::SwiftImportStatus, skipped: bool) {
        let services = &self.shared.services;
        self.onboarding_command(|inner| {
            let key = self.read_key();
            inner
                .onboarding
                .finish_import(status, skipped, &inner.store, services, key);
            self.reload_sections(inner);
            inner.phones.load(services);
            inner.phones.refresh(services);
            inner.app.phone = Self::phone_card(services);
            inner.publisher.schedule(BridgeTopic::App);
        });
    }

    /// Page 1 moves on by itself once every step is handled, nothing is
    /// left to download and the vault row is neither checking nor refused.
    /// Only after a command that can change a step, so Back from page 2
    /// stays on page 1.
    fn advance_if_handled(&self, inner: &mut Inner) {
        if inner.onboarding.page == steno_bridge::OnboardingPage::Permissions
            && inner.onboarding.permissions_handled()
            && !inner.onboarding.models_pending(&self.shared.services)
            && !inner.onboarding.vault_pending()
        {
            inner.onboarding.advance(&self.shared.services);
        }
    }

    /// The page's option back into the model's: a person is looked up
    /// among the picker's people; a create row carries its name.
    fn model_option(option: &SpeakerOption, inner: &Inner) -> Outcome<ModelOption> {
        match option.kind {
            SpeakerOptionKind::Person => {
                let detail = inner.detail.as_ref().ok_or_else(no_selection)?;
                let speakers = &detail.speakers;
                let known = speakers.persons.iter().chain(speakers.recent.iter()).chain(
                    speakers
                        .export
                        .iter()
                        .flat_map(|export| export.persons.iter()),
                );
                let person = option
                    .person_id
                    .and_then(|id| known.into_iter().find(|person| person.id == id))
                    .ok_or_else(|| BridgeError::not_found("No person with that id."))?;
                Ok(ModelOption {
                    kind: OptionKind::Person(person.clone()),
                    tag: None,
                })
            }
            SpeakerOptionKind::Create => {
                let name = option.label.trim();
                if name.is_empty() {
                    return Err(BridgeError::invalid_params("A new person needs a name."));
                }
                Ok(ModelOption {
                    kind: OptionKind::Create(name.to_owned()),
                    tag: None,
                })
            }
            SpeakerOptionKind::Unknown => Err(BridgeError::failed(
                "Marking a speaker as unknown is not available yet.",
            )),
        }
    }
}

impl Inner {
    fn subtitle(&self, section: SettingsSection) -> &str {
        self.subtitles.get(&section).map_or("", String::as_str)
    }

    /// The Summaries model the window owns.
    fn llm_mut(&mut self, window: BridgeWindow) -> &mut LlmSettingsViewModel {
        if window == BridgeWindow::Onboarding {
            &mut self.onboarding.llm
        } else {
            &mut self.llm
        }
    }
}

fn wire_option(option: &ModelOption) -> SpeakerOption {
    match &option.kind {
        OptionKind::Person(person) => SpeakerOption {
            kind: SpeakerOptionKind::Person,
            label: person.display_name.clone(),
            detail: option.tag.map(|tag| tag.as_str().to_owned()),
            person_id: Some(person.id),
        },
        OptionKind::Create(name) => SpeakerOption {
            kind: SpeakerOptionKind::Create,
            label: name.clone(),
            detail: option.tag.map(|tag| tag.as_str().to_owned()),
            person_id: None,
        },
    }
}

/// The entry a confirmed delete forgot
/// ([`Recorder::forget_recording`](crate::services::Recorder::forget_recording)),
/// recorded again on drop unless the rows went (`folder` taken), so a
/// refused, uncommitted or panicked delete keeps what lets a launch adopt
/// the master. A row already gone records nothing. Read and restore hold
/// the host lock; dropped with it released.
struct ForgottenEntry<'a> {
    host: &'a Host,
    meeting_id: Uuid,
    folder: Option<PathBuf>,
}

impl Drop for ForgottenEntry<'_> {
    fn drop(&mut self) {
        let Some(folder) = self.folder.take() else {
            return;
        };
        let inner = self.host.lock();
        if !matches!(inner.store.meeting(self.meeting_id), Ok(None)) {
            self.host
                .shared
                .services
                .recorder
                .restore_recording(self.meeting_id, &folder);
        }
    }
}

#[allow(clippy::too_many_lines)]
impl BridgeHost for Host {
    fn page_ready(&self) -> Outcome<()> {
        self.lock().publisher.page_did_become_ready();
        self.shared.feed.page_did_become_ready();
        self.publish();
        if self.window == BridgeWindow::Onboarding {
            self.check_vault_if_due();
        }
        Ok(())
    }

    fn page_layout(&self, _params: PageLayoutParams) -> Outcome<()> {
        Ok(())
    }

    fn meetings_set_filter(&self, params: SetFilterParams) -> Outcome<()> {
        self.command(&[BridgeTopic::MeetingsList], |inner| {
            inner.list.set_state_filter(params.filter);
        });
        Ok(())
    }

    fn meetings_set_tag_filter(&self, params: SetTagFilterParams) -> Outcome<()> {
        self.command(&[BridgeTopic::MeetingsList], |inner| {
            inner.list.set_tag_filter(params.tag);
        });
        Ok(())
    }

    fn meetings_set_query(&self, params: SetQueryParams) -> Outcome<()> {
        self.command(&[BridgeTopic::MeetingsList], |inner| {
            inner.list.set_query(params.query, &inner.store);
        });
        Ok(())
    }

    fn meetings_select(&self, params: MeetingIdParams) -> Outcome<()> {
        self.command(&[BridgeTopic::MeetingsList], |inner| {
            if inner.list.contains(params.meeting_id) {
                inner.pending_selection = None;
                inner.list.selection = Some(params.meeting_id);
            } else {
                // A deep link ahead of its row: selected when the row lands.
                inner.pending_selection = Some(params.meeting_id);
            }
        });
        Ok(())
    }

    fn meetings_delete(&self, params: MeetingIdParams) -> Outcome<ConfirmReply> {
        let recorder_idle = self.shared.services.recorder.status().state == RecordingState::Idle;
        let (prompt, recording) = {
            let inner = self.lock();
            let meeting = inner
                .list
                .all
                .iter()
                .find(|meeting| meeting.id == params.meeting_id)
                .ok_or_else(no_such_meeting)?;
            let recording = meeting.state.kind() == steno_core::MeetingStateKind::Recording;
            if !MeetingListViewModel::can_delete(meeting, recorder_idle) {
                let message = if recording {
                    STILL_RECORDING
                } else {
                    "This meeting is still being processed."
                };
                return Err(BridgeError::failed(message));
            }
            let prompt = ConfirmDestructiveParams {
                title: format!(
                    "Delete “{}”?",
                    crate::labels::display_title(meeting, self.now(), self.shared.config.zone)
                ),
                message: self
                    .shared
                    .config
                    .platform
                    .mac_or(DELETE_MEETING_MESSAGE, DELETE_MEETING_MESSAGE_ELSEWHERE)
                    .to_owned(),
                confirm_title: "Delete".to_owned(),
            };
            (prompt, recording)
        };
        // Where the meeting's folder may be, for a meeting left `recording`
        // (another process may still record it) or one no asset names.
        // Read with the lock released.
        let named = !recording && matches!(self.shared.store.asset(params.meeting_id), Ok(Some(_)));
        let left = (!named).then(|| {
            self.shared
                .services
                .recorder
                .left_recording(params.meeting_id)
        });
        if recording && left.as_ref().is_some_and(|left| left.still_written) {
            return Err(BridgeError::failed(STILL_RECORDING));
        }
        let confirmed = self.confirm(&prompt);
        if confirmed {
            // Before the rows go, so the launch's adoption never finds the
            // meeting gone and its master still there; an entry that stays
            // refuses the delete before any row goes.
            let Ok(folder) = self
                .shared
                .services
                .recorder
                .forget_recording(params.meeting_id)
            else {
                tracing::warn!(
                    meeting_id = %params.meeting_id,
                    "a meeting was not deleted: its recording's folder could not be forgotten"
                );
                return Err(BridgeError::failed(COULD_NOT_DELETE));
            };
            let mut forgotten = ForgottenEntry {
                host: self,
                meeting_id: params.meeting_id,
                folder,
            };
            let now = self.now();
            self.command(
                &[BridgeTopic::MeetingsList, BridgeTopic::Progress],
                |inner| {
                    let deleted = inner.list.delete(
                        params.meeting_id,
                        &inner.store,
                        &*self.shared.services.file_system,
                        left.as_ref(),
                        recording,
                    );
                    inner.list.reload(&inner.store);
                    // Posted only when the rows went: the progress entry goes
                    // and the forgotten entry is not recorded again.
                    if deleted {
                        forgotten.folder = None;
                        inner.progress.apply(
                            &MeetingEvent::Deleted {
                                meeting_id: params.meeting_id,
                            },
                            now,
                        );
                    }
                },
            );
        }
        Ok(ConfirmReply { confirmed })
    }

    fn meeting_set_tab(&self, params: SetTabParams) -> Outcome<()> {
        self.detail_command(|detail| detail.tab = params.tab)
    }

    fn meeting_set_tags(&self, params: SetTagsParams) -> Outcome<()> {
        let tags = MeetingDetailViewModel::tags_from(&params.tags);
        let now = self.now();
        self.detail_write(|detail, store| detail.set_tags(tags, store, now))
    }

    fn meeting_set_template(&self, params: SetTemplateParams) -> Outcome<()> {
        let now = self.now();
        self.detail_write(|detail, store| {
            detail.set_template(
                &params.template_id,
                store,
                now,
                &*self.shared.services.pipeline,
            );
        })
    }

    fn meeting_rerun_summary(&self) -> Outcome<()> {
        self.detail_write(|detail, _| detail.rerun_summary(&*self.shared.services.pipeline))
    }

    fn meeting_reexport(&self) -> Outcome<()> {
        self.detail_write(|detail, _| detail.reexport(&*self.shared.services.pipeline))
    }

    /// "Process again" on the selected meeting, after
    /// [`process_again_prompt`] when needed. A decline changes nothing;
    /// when the selection moved while the prompt was up, nothing starts.
    fn meeting_process_again(&self) -> Outcome<()> {
        let (meeting_id, asks) = {
            let inner = self.lock();
            let detail = inner.detail.as_ref().ok_or_else(no_selection)?;
            (detail.id, detail.process_again_asks_first())
        };
        if asks && !self.confirm(&process_again_prompt()) {
            return Ok(());
        }
        let platform = self.shared.config.platform;
        self.detail_write(|detail, _| {
            if detail.id == meeting_id {
                detail.process_again(&*self.shared.services.pipeline, platform);
            }
        })
    }

    fn meeting_set_keep_audio(&self, params: SetBoolParams) -> Outcome<ConfirmReply> {
        let (meeting_id, confirming) = {
            let inner = self.lock();
            let detail = inner.detail.as_ref().ok_or_else(no_selection)?;
            (detail.id, !params.value && detail.would_delete_now())
        };
        self.set_keep_audio(params.value, meeting_id, confirming)
    }

    fn meeting_delete_recording_now(&self) -> Outcome<ConfirmReply> {
        let meeting_id = self.lock().detail.as_ref().ok_or_else(no_selection)?.id;
        self.set_keep_audio(false, meeting_id, true)
    }

    fn meeting_save_notes(&self, params: SaveNotesParams) -> Outcome<()> {
        // The text is written to the meeting it names, selected or not.
        let now = self.now();
        self.shared
            .store
            .update_meeting(params.meeting_id, now, |meeting| {
                meeting.scratchpad = params.text;
                Ok(())
            })?;
        self.command(&[], |inner| self.reload_detail(inner));
        Ok(())
    }

    fn meeting_reveal_recording(&self) -> Outcome<()> {
        let inner = self.lock();
        let detail = inner.detail.as_ref().ok_or_else(no_selection)?;
        let path = detail
            .export
            .as_ref()
            .and_then(|export| export.audio.as_ref())
            .and_then(|asset| file_url_path(&asset.url))
            .filter(|_| detail.recording_files_exist)
            .ok_or_else(|| {
                BridgeError::not_found(self.shared.config.platform.mac_or(
                    "The recording is no longer on this Mac.",
                    "The recording is no longer on this computer.",
                ))
            })?;
        self.shared.services.opener.reveal(&path);
        Ok(())
    }

    fn meeting_reveal_export(&self) -> Outcome<()> {
        let inner = self.lock();
        let detail = inner.detail.as_ref().ok_or_else(no_selection)?;
        let folder = detail
            .deliveries
            .iter()
            .filter_map(|delivery| delivery.receipt.as_ref())
            .map(|receipt| Path::new(&receipt.root).join(&receipt.folder))
            .next()
            .ok_or_else(|| BridgeError::not_found("Nothing has been exported yet."))?;
        self.shared.services.opener.reveal(&folder);
        Ok(())
    }

    fn speakers_options(&self, params: SpeakerOptionsParams) -> Outcome<SpeakerOptionsReply> {
        let inner = self.lock();
        let detail = inner.detail.as_ref().ok_or_else(no_selection)?;
        Ok(SpeakerOptionsReply {
            prefill: detail.speakers.prefill(params.speaker_id),
            options: detail
                .speakers
                .options(params.speaker_id, &params.query)
                .iter()
                .map(wire_option)
                .collect(),
        })
    }

    fn speakers_select(&self, params: SelectSpeakerParams) -> Outcome<()> {
        let now = self.now();
        let outcome = {
            let mut inner = self.lock();
            let option = Self::model_option(&params.option, &inner)?;
            let outcome = Self::with_detail(&mut inner, |detail, store| {
                if detail
                    .speakers
                    .select(&option, params.speaker_id, store, now)
                {
                    detail.speakers_changed();
                }
                // The host cannot see the picker close; one re-export per
                // change is idempotent.
                detail.picker_closed(&*self.shared.services.pipeline);
            });
            self.reload_detail(&mut inner);
            outcome
        };
        self.publish();
        outcome
    }

    fn speakers_play(&self, params: SpeakerIdParams) -> Outcome<()> {
        self.detail_command(|detail| {
            detail
                .speakers
                .play(params.speaker_id, &*self.shared.services.clip_player);
        })
    }

    fn speakers_stop(&self) -> Outcome<()> {
        self.detail_command(|detail| {
            detail
                .speakers
                .stop_playback(&*self.shared.services.clip_player);
        })
    }

    fn recording_start(&self, params: StartRecordingParams) -> Outcome<()> {
        // The recorder starts, then the live row is requested so the window
        // selects it. A failed start re-reads the permissions.
        let recorder = &self.shared.services.recorder;
        recorder.start(params.mode, None);
        let status = recorder.status();
        if status.state == RecordingState::Idle {
            recorder.refresh_permissions();
        }
        self.shared.feed.poke();
        self.command(&[BridgeTopic::App], |inner| {
            if status.state == RecordingState::Recording {
                inner.app.requested_meeting_id = status.meeting_id;
            }
        });
        Ok(())
    }

    fn recording_stop(&self) -> Outcome<()> {
        self.shared.services.recorder.stop();
        self.recorder_changed();
        Ok(())
    }

    fn recording_toggle(&self) -> Outcome<()> {
        self.shared.services.recorder.toggle();
        self.recorder_changed();
        Ok(())
    }

    fn recording_keep_going(&self) -> Outcome<()> {
        self.shared.services.recorder.keep_recording();
        self.recorder_changed();
        Ok(())
    }

    fn recording_clear_messages(&self) -> Outcome<()> {
        self.shared.services.recorder.clear_messages();
        self.recorder_changed();
        Ok(())
    }

    /// "Not now" hides the setup banner for this launch; OK on the engine
    /// notice clears [`engine_notice::PENDING_KEY`] so it never shows again.
    fn setup_dismiss_banner(&self) -> Outcome<()> {
        self.command(&[BridgeTopic::App], |inner| {
            if inner.app.speech_engine_notice {
                self.shared
                    .services
                    .preferences
                    .set_flag(engine_notice::PENDING_KEY, false);
                inner.app.speech_engine_notice = false;
            } else {
                inner.app.setup_banner_dismissed = true;
            }
        });
        Ok(())
    }

    fn settings_general_set_launch_at_login(&self, params: SetBoolParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsGeneral, |inner| {
            inner
                .general
                .set_launch_at_login(params.value, &inner.store, &self.shared.services);
        });
        Ok(())
    }

    fn settings_general_set_detection(&self, params: SetBoolParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsGeneral, |inner| {
            inner
                .general
                .set_detection_enabled(params.value, &inner.store);
        });
        Ok(())
    }

    fn settings_general_set_default_template(&self, params: SetTemplateParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsGeneral, |inner| {
            inner
                .general
                .set_default_template(&params.template_id, &inner.store);
        });
        Ok(())
    }

    fn settings_general_request_calendar(&self) -> Outcome<()> {
        self.outside_lock(
            BridgeTopic::SettingsGeneral,
            |inner| {
                inner.general.begin_calendar_request();
                Some(())
            },
            |()| {
                self.shared
                    .services
                    .permissions
                    .request(PermissionKind::Calendar)
            },
            |inner, state| inner.general.finish_calendar_request(state),
        );
        Ok(())
    }

    fn settings_general_set_automatic_updates(
        &self,
        params: SetAutomaticUpdatesParams,
    ) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsGeneral, |_| {
            self.shared
                .services
                .updater
                .set_automatically_checks(params.automatically_checks);
            self.shared
                .services
                .updater
                .set_automatically_downloads(params.automatically_downloads);
        });
        Ok(())
    }

    fn settings_general_open_login_items(&self) -> Outcome<()> {
        self.shared.services.login_item.open_system_settings();
        Ok(())
    }

    fn settings_recording_set_input_device(&self, params: SetStringParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsRecording, |inner| {
            let uid = (!params.value.is_empty()).then_some(params.value);
            inner.audio.set_input_device(uid, &inner.store);
        });
        Ok(())
    }

    fn settings_recording_refresh_devices(&self) -> Outcome<()> {
        // Listed before the lock: on Linux a list may wait for PipeWire.
        let listed = self.shared.services.audio_devices.inputs();
        self.settings_command(BridgeTopic::SettingsRecording, |inner| {
            inner.audio.apply_devices(listed);
        });
        Ok(())
    }

    fn settings_recording_choose_folder(&self) -> Outcome<ChosenPathReply> {
        let current = self.lock().audio.audio_folder.clone();
        Ok(self.choose(Some(&current), |folder| {
            self.settings_command(BridgeTopic::SettingsRecording, |inner| {
                inner
                    .audio
                    .set_audio_folder(folder, &inner.store, &self.shared.services);
            });
        }))
    }

    fn settings_recording_reveal_folder(&self) -> Outcome<()> {
        self.lock().audio.reveal_folder(&self.shared.services);
        Ok(())
    }

    fn settings_recording_set_retention(&self, params: SetRetentionParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsRecording, |inner| {
            inner.audio.set_retention(
                params.retention.mode,
                params.retention.days,
                &inner.store,
                &self.shared.services,
            );
        });
        Ok(())
    }

    fn settings_recording_request_permission(&self, params: PermissionKindParams) -> Outcome<()> {
        let kind = params.kind;
        if self.waits_for_recording(kind) {
            self.settings_command(BridgeTopic::SettingsRecording, |inner| {
                inner.audio.waits_for_recording = Some(kind);
            });
            return Ok(());
        }
        self.outside_lock(
            BridgeTopic::SettingsRecording,
            |inner| inner.audio.begin_request(kind).then_some(()),
            |()| self.request_unless_recording(kind),
            |inner, (state, waited)| {
                inner.audio.finish_request(kind, state);
                if waited {
                    inner.audio.waits_for_recording = Some(kind);
                }
            },
        );
        Ok(())
    }

    fn settings_transcription_set_engine(&self, params: SetStringParams) -> Outcome<()> {
        let engine: SpeechEngineId = params.value.parse().map_err(|_| {
            BridgeError::invalid_params(format!("Unknown speech engine {}.", params.value))
        })?;
        self.settings_command(BridgeTopic::SettingsTranscription, |inner| {
            inner
                .speech
                .set_engine(engine, &inner.store, &self.shared.services);
        });
        Ok(())
    }

    fn settings_transcription_download(&self, params: AssetIdParams) -> Outcome<()> {
        let asset = parse_asset(&params)?;
        // The reply returns at once; the download publishes from its own
        // thread. A download of the asset still running is reattached to.
        let starts = {
            let mut inner = self.lock();
            let starts = inner.speech.begin_download(asset);
            for topic in MODEL_TOPICS {
                inner.publisher.schedule(topic);
            }
            starts
        };
        self.publish();
        if !starts {
            return Ok(());
        }
        let host = self.clone();
        let spawned = thread::Builder::new()
            .name(format!("steno-download-{}", asset.as_str()))
            .spawn(move || host.run_download(asset));
        if let Err(error) = spawned {
            let message = format!("The download could not start: {error}");
            self.models_command(|inner| {
                inner.speech.finish_download(
                    asset,
                    Err(message.clone().into()),
                    &self.shared.services,
                );
            });
            return Err(BridgeError::failed(message));
        }
        Ok(())
    }

    fn settings_transcription_remove(&self, params: AssetIdParams) -> Outcome<()> {
        let asset = parse_asset(&params)?;
        self.models_command(|inner| inner.speech.remove(asset, &self.shared.services));
        Ok(())
    }

    /// Switches to the Neural Engine's model; a meeting waiting for the
    /// sidecar's models stops saying the Neural Engine is off. A failed
    /// rebuild leaves both until a reload succeeds.
    fn settings_transcription_use_neural_engine(&self) -> Outcome<()> {
        self.models_command(|inner| inner.speech.use_neural_engine(&self.shared.services));
        Ok(())
    }

    // settings.summaries: the four form commands answer on the calling
    // window's model, the rest on the Settings section.

    fn settings_summaries_select_preset(&self, params: SetStringParams) -> Outcome<()> {
        let preset: LlmPreset = params.value.parse().map_err(|_| {
            BridgeError::invalid_params(format!("Unknown summaries service {}.", params.value))
        })?;
        let now = self.now();
        self.summaries_command(self.window, |llm, store| {
            llm.select_preset(preset, store, &self.shared.services, now);
        });
        Ok(())
    }

    fn settings_summaries_update(&self, params: SummariesUpdateParams) -> Outcome<()> {
        self.summaries_command(self.window, |llm, _| llm.apply_update(&params));
        Ok(())
    }

    fn settings_summaries_save(&self) -> Outcome<()> {
        let now = self.now();
        self.summaries_command(BridgeWindow::Settings, |llm, store| {
            llm.commit(store, &self.shared.services, now);
        });
        Ok(())
    }

    fn settings_summaries_test(&self) -> Outcome<()> {
        self.summaries_command(self.window, |llm, _| llm.request_probe());
        Ok(())
    }

    /// Saves the confirmation under the lock, fetches the model list with
    /// it released, then saves and probes the pick.
    fn settings_summaries_confirm_codex(&self) -> Outcome<()> {
        let services = &self.shared.services;
        self.outside_lock(
            BridgeTopic::SettingsSummaries,
            |inner| {
                let fetch = inner
                    .llm
                    .begin_confirm_codex(&inner.store, services, self.now());
                self.follow_settings(inner, false);
                fetch.then_some(())
            },
            |()| (services.llm.codex_account(), services.llm.codex_models()),
            |inner, (account, models)| {
                inner
                    .llm
                    .finish_confirm_codex(account, models, &inner.store, services, self.now());
                self.follow_settings(inner, false);
            },
        );
        self.run_pending_probe(BridgeWindow::Settings);
        Ok(())
    }

    fn settings_summaries_refresh_codex_status(&self) -> Outcome<()> {
        // A file read, outside the lock all the same.
        let account = self.shared.services.llm.codex_account();
        self.summaries_command(self.window, |llm, _| llm.apply_codex_account(account));
        Ok(())
    }

    fn settings_summaries_refresh_codex_models(&self) -> Outcome<()> {
        let llm = &self.shared.services.llm;
        self.outside_lock(
            BridgeTopic::SettingsSummaries,
            |inner| inner.llm.begin_codex_models().then_some(()),
            |()| (llm.codex_account(), llm.codex_models()),
            |inner, (account, models)| inner.llm.finish_codex_models(account, models),
        );
        Ok(())
    }

    fn settings_summaries_select_codex_model(&self, params: SetStringParams) -> Outcome<()> {
        let now = self.now();
        self.summaries_command(BridgeWindow::Settings, |llm, store| {
            llm.select_codex_model(&params.value, store, &self.shared.services, now);
        });
        Ok(())
    }

    fn settings_summaries_stop_using_codex(&self) -> Outcome<()> {
        let now = self.now();
        self.summaries_command(BridgeWindow::Settings, |llm, store| {
            llm.stop_using_codex(store, &self.shared.services, now);
        });
        Ok(())
    }

    fn settings_export_set_enabled(&self, params: SetBoolParams) -> Outcome<()> {
        self.export_command(|obsidian| obsidian.set_enabled(params.value));
        Ok(())
    }

    fn settings_export_choose_vault(&self) -> Outcome<ChosenPathReply> {
        let current = self.lock().obsidian.vault_url();
        Ok(self.choose(current.as_deref(), |folder| {
            self.export_command(|obsidian| obsidian.choose_vault(folder));
        }))
    }

    fn settings_export_update(&self, params: ExportUpdateParams) -> Outcome<()> {
        self.export_command(|obsidian| {
            if let Some(people) = params.people_folder {
                obsidian.people_folder = people;
            }
            if let Some(tag) = params.task_tag {
                obsidian.task_tag = tag;
            }
            params
                .include_audio
                .and_then(|include| obsidian.set_include_audio(include))
        });
        Ok(())
    }

    fn settings_export_save(&self) -> Outcome<()> {
        self.export_command(ObsidianSettingsViewModel::commit);
        Ok(())
    }

    fn settings_phone_begin_pairing(&self) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsPhone, |inner| {
            inner.phones.begin_pairing(&self.shared.services);
        });
        Ok(())
    }

    fn settings_phone_cancel_pairing(&self) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsPhone, |inner| {
            inner.phones.cancel_pairing(&self.shared.services);
        });
        Ok(())
    }

    fn settings_phone_revoke(&self, params: DeviceIdParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsPhone, |inner| {
            inner.phones.revoke(params.device_id, &self.shared.services);
        });
        Ok(())
    }

    /// The page sees `busy` while a keychain prompt may be up, the handover
    /// reads its identity again with the lock released, then the outcome
    /// lands. A second Try again or Pair again while one runs does nothing.
    fn settings_phone_try_again(&self) -> Outcome<()> {
        let Some(handover) = self.shared.services.handover.clone() else {
            return Ok(());
        };
        if !self.begin_phones_busy() {
            return Ok(());
        }
        let outcome = handover.try_again();
        self.finish_phones_busy(|phones, services| phones.finish_try_again(outcome, services));
        Ok(())
    }

    /// Asks [`pair_again_prompt`] while the section is busy, then runs as
    /// [`Self::settings_phone_try_again`]. Once the handover no longer
    /// waits, nothing is asked and the section is refreshed. The reply says
    /// whether the user went ahead.
    fn settings_phone_pair_again(&self) -> Outcome<ConfirmReply> {
        let Some(handover) = self.shared.services.handover.clone() else {
            return Ok(ConfirmReply { confirmed: false });
        };
        if handover.waiting().is_none() {
            self.phones_changed();
            return Ok(ConfirmReply { confirmed: false });
        }
        if !self.begin_phones_busy() {
            return Ok(ConfirmReply { confirmed: false });
        }
        if !self.confirm(&pair_again_prompt(self.shared.config.platform)) {
            self.change_phones(PhonesSettingsViewModel::cancel_busy);
            return Ok(ConfirmReply { confirmed: false });
        }
        let outcome = handover.pair_again();
        self.finish_phones_busy(|phones, services| phones.finish_pair_again(outcome, services));
        Ok(ConfirmReply { confirmed: true })
    }

    fn onboarding_request(&self, params: PermissionKindParams) -> Outcome<()> {
        let kind = params.kind;
        if self.waits_for_recording(kind) {
            self.onboarding_command(|inner| inner.onboarding.waits_for_recording = Some(kind));
            return Ok(());
        }
        // Three steps so the page sees `isRequesting` while the prompt is up.
        self.onboarding_command(|inner| inner.onboarding.begin_request(kind));
        let (state, waited) = self.request_unless_recording(kind);
        self.onboarding_command(|inner| {
            inner.onboarding.finish_request(kind, state);
            if waited {
                inner.onboarding.waits_for_recording = Some(kind);
            }
            self.advance_if_handled(inner);
        });
        Ok(())
    }

    fn onboarding_skip(&self, params: PermissionKindParams) -> Outcome<()> {
        self.onboarding_command(|inner| {
            inner.onboarding.skip(params.kind);
            self.advance_if_handled(inner);
        });
        Ok(())
    }

    fn onboarding_refresh(&self) -> Outcome<()> {
        self.onboarding_command(|inner| {
            // Loaded at start, so the key is not read again.
            inner
                .onboarding
                .load(&inner.store, &self.shared.services, KeyRead::Absent);
            inner.onboarding.retry_vault_check();
            self.advance_if_handled(inner);
        });
        Ok(())
    }

    fn onboarding_advance(&self) -> Outcome<()> {
        self.onboarding_command(|inner| inner.onboarding.advance(&self.shared.services));
        Ok(())
    }

    fn onboarding_back(&self) -> Outcome<()> {
        self.onboarding_command(|inner| inner.onboarding.back());
        Ok(())
    }

    fn onboarding_save_summaries(&self) -> Outcome<()> {
        let now = self.now();
        self.onboarding_command(|inner| {
            inner
                .onboarding
                .save_summaries(&inner.store, &self.shared.services, now);
        });
        Ok(())
    }

    /// As [`Self::settings_summaries_confirm_codex`], on onboarding's
    /// Summaries row.
    fn onboarding_confirm_summaries_with_codex(&self) -> Outcome<()> {
        let services = &self.shared.services;
        self.outside_lock(
            BridgeTopic::Onboarding,
            |inner| {
                let fetch = inner.onboarding.begin_confirm_summaries_with_codex(
                    &inner.store,
                    services,
                    self.now(),
                );
                self.follow_settings(inner, true);
                fetch.then_some(())
            },
            |()| (services.llm.codex_account(), services.llm.codex_models()),
            |inner, (account, models)| {
                inner.onboarding.finish_confirm_summaries_with_codex(
                    account,
                    models,
                    &inner.store,
                    services,
                    self.now(),
                );
                self.follow_settings(inner, true);
            },
        );
        self.run_pending_probe(BridgeWindow::Onboarding);
        Ok(())
    }

    fn onboarding_choose_vault(&self) -> Outcome<ChosenPathReply> {
        let current = self.lock().onboarding.obsidian.vault_url();
        Ok(self.choose(current.as_deref(), |folder| {
            self.onboarding_vault_command(|onboarding| onboarding.choose_vault(folder));
        }))
    }

    fn onboarding_save_vault(&self) -> Outcome<()> {
        self.onboarding_vault_command(OnboardingViewModel::begin_save_vault);
        Ok(())
    }

    fn onboarding_skip_setup(&self, params: SetupStepParams) -> Outcome<()> {
        self.onboarding_command(|inner| {
            inner
                .onboarding
                .skip_setup(params.step, &self.shared.services);
        });
        Ok(())
    }

    fn onboarding_finish(&self) -> Outcome<()> {
        self.onboarding_command(|inner| inner.onboarding.finish(&self.shared.services));
        Ok(())
    }

    /// The page sees `importing` while the keychain prompts are up, the
    /// import runs with the lock released, then the outcome lands. A second
    /// Continue or Try again while one runs does nothing.
    fn onboarding_import(&self) -> Outcome<()> {
        let Some(import) = self.shared.services.swift_import.clone() else {
            return Ok(());
        };
        if !self.onboarding_command(|inner| inner.onboarding.begin_import()) {
            return Ok(());
        }
        let status = import.run();
        self.finish_swift_import(status, false);
        Ok(())
    }

    /// Not now while a run's prompts are up does nothing: that run
    /// publishes the outcome.
    fn onboarding_skip_import(&self) -> Outcome<()> {
        let Some(import) = self.shared.services.swift_import.clone() else {
            return Ok(());
        };
        if self.lock().onboarding.importing {
            return Ok(());
        }
        let status = import.skip();
        self.finish_swift_import(status, true);
        Ok(())
    }

    /// On the Mac only: no other platform asks for a folder.
    fn onboarding_open_files_and_folders(&self) -> Outcome<()> {
        if self.shared.config.platform == Platform::Macos {
            self.shared.services.opener.open_url(FILES_AND_FOLDERS_URL);
        }
        Ok(())
    }

    fn updates_check(&self) -> Outcome<()> {
        if !self.shared.config.updates_managed {
            self.shared.services.updater.check_for_updates();
        }
        self.settings_command(BridgeTopic::SettingsGeneral, |_| {});
        Ok(())
    }

    fn system_open_url(&self, params: OpenUrlParams) -> Outcome<()> {
        // Only `https:` and `mailto:`: a page cannot open files, plain
        // `http:` or run scripts.
        let lower = params.url.to_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("mailto:")) {
            return Err(BridgeError::invalid_params(
                "Only https: and mailto: links open from the page.",
            ));
        }
        self.shared.services.opener.open_url(&params.url);
        Ok(())
    }

    fn system_open_system_settings(&self, params: PermissionKindParams) -> Outcome<()> {
        self.shared
            .services
            .permissions
            .open_system_settings(params.kind);
        Ok(())
    }

    fn window_open(&self, params: WindowParams) -> Outcome<()> {
        self.command(&[BridgeTopic::App], |inner| match params.window {
            BridgeWindow::Main => {
                if let Some(meeting_id) = params.meeting_id {
                    inner.app.requested_meeting_id = Some(meeting_id);
                }
            }
            BridgeWindow::Settings => {
                if let Some(section) = params.section {
                    inner.app.requested_settings_section = Some(section);
                }
            }
            BridgeWindow::Onboarding => {}
        });
        self.shared.services.opener.open_window(params.window);
        Ok(())
    }

    fn window_close(&self, params: WindowParams) -> Outcome<()> {
        if params.window != BridgeWindow::Onboarding {
            return Err(BridgeError::invalid_params(
                "The onboarding window closes only itself.",
            ));
        }
        self.shared.services.opener.close_window(params.window);
        Ok(())
    }

    fn ui_confirm_destructive(&self, params: ConfirmDestructiveParams) -> Outcome<ConfirmReply> {
        Ok(ConfirmReply {
            confirmed: self.confirm(&params),
        })
    }
}

fn parse_asset(params: &AssetIdParams) -> Outcome<ModelAsset> {
    params
        .asset_id
        .parse()
        .map_err(|_| BridgeError::invalid_params(format!("Unknown model {}.", params.asset_id)))
}

/// A poisoned lock is reused: a panic in one command must not take the
/// host down with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use steno_bridge::BridgeTopic;
    use steno_core::Store;
    use uuid::Uuid;

    use super::{Host, HostConfig, block_on};
    use crate::fakes::FakeServices;
    use crate::services::Clock;

    /// A boundary that sleeps needs the host runtime's timer, here and on
    /// the helper thread.
    async fn a_boundary_with_a_timeout() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    #[test]
    fn a_boundary_may_use_the_runtimes_timer() {
        block_on(a_boundary_with_a_timeout());
    }

    #[tokio::test]
    async fn a_boundary_may_use_the_timer_from_inside_a_runtime() {
        block_on(a_boundary_with_a_timeout());
    }

    /// A commit that lands while [`Host::new`] loads its view models
    /// reaches the list at the first pass: the change mark is read before
    /// the loads.
    #[test]
    fn a_commit_during_the_hosts_loads_shows_at_the_first_pass() {
        /// Saves a new meeting on each reading while `committing`.
        struct CommittingClock {
            store: Arc<Store>,
            now: DateTime<Utc>,
            committing: AtomicBool,
            saved: Mutex<Vec<Uuid>>,
        }

        impl Clock for CommittingClock {
            fn now(&self) -> DateTime<Utc> {
                if self.committing.load(Ordering::SeqCst) {
                    let mut meeting = steno_core::testing::sample_data::meeting();
                    meeting.id = Uuid::new_v4();
                    self.store.save_meeting(&meeting).unwrap();
                    self.saved.lock().unwrap().push(meeting.id);
                }
                self.now
            }
        }

        let now = steno_core::json::parse_date("2026-09-29T12:50:00.000Z").unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let clock = Arc::new(CommittingClock {
            store: store.clone(),
            now,
            committing: AtomicBool::new(true),
            saved: Mutex::default(),
        });
        let mut services = FakeServices::new(now).services();
        services.clock = clock.clone();
        let host = Host::new(store, services, HostConfig::default()).unwrap();
        clock.committing.store(false, Ordering::SeqCst);
        let saved = clock.saved.lock().unwrap().clone();
        assert!(!saved.is_empty(), "the clock committed during the loads");

        host.reload_pass();
        let list = host
            .snapshot(BridgeTopic::MeetingsList)
            .unwrap()
            .to_string()
            .to_lowercase();
        for id in saved {
            assert!(list.contains(&id.to_string()), "{id} listed: {list}");
        }
    }
}
