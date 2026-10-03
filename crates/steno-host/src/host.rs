//! The bridge host over the real store: one [`Host`] owns every view model
//! of the three windows and answers all 75 methods, after
//! `Web/{MainWindowBridge,SettingsBridge,OnboardingBridge}.swift` and the
//! controller state of `AppController.swift` the bridges read. Commands
//! map one to one onto the view models' methods, so no rule lives here;
//! the destructive ones ask through the shell's `confirm` first and the
//! folder choosers through `choose_folder`, the two native surfaces the
//! page cannot draw.
//!
//! One host for the three windows where Swift had three: the Tauri shell
//! has one event channel per window and routes a published topic to the
//! window that shows it (`app` to the main and the Settings window). The
//! Swift hosts loaded their view models when a window opened; this one
//! loads them all at construction, so a window that opens later publishes
//! at once on its `page.ready`, and it re-reads the Settings sections when
//! the stored settings change under them ([`Host::store_changed`], a write
//! from the onboarding window), which is what reopening the Swift window
//! did.
//!
//! # Which window is calling
//!
//! The shell answers each window's commands through [`Host::for_window`],
//! a clone of the host bound to that window (`Dispatcher::new(host
//! .for_window(BridgeWindow::Onboarding))`). Four methods depend on it: the
//! Summaries form sends the Settings window's method names
//! (`settings.summaries.selectPreset`, `.update`, `.test`,
//! `.refreshCodexStatus`) from both windows, and each window answers them
//! on its own `LlmSettingsViewModel` and republishes its own topic, as
//! `SummariesCommands` did in Swift. Every other method answers the same
//! from any window. A host that was never bound ([`Host::new`]) answers as
//! the main and Settings windows do.
//!
//! # Publishing
//!
//! Every command publishes the topics it changed before it returns, and
//! the page treats each snapshot as a full state, so the order in which
//! the reply and the snapshots arrive decides nothing (the dispatcher's
//! doc says why they can cross). Snapshots are built under the host's
//! lock and emitted after it is released, in build order (one publisher
//! runs at a time), so a sink may read [`Host::snapshot`] from inside
//! `emit`, and a slow sink never holds a command or the recorder. A sink
//! must not run a command from inside `emit`.
//!
//! The throttled `recording` topic is flushed by a thread the host owns:
//! armed when a publish leaves it pending, asleep otherwise, gone with the
//! last clone of the host. The shell has no timer to run for it. The one
//! poll the shell drives is the pairing poll, [`Host::refresh_pairing`],
//! every two seconds while a code is shown, as the plan says.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock, RwLockReadGuard, Weak};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, Utc};
use serde_json::{Value, to_value};
use steno_bridge::{
    AppPhone, AssetIdParams, BridgeError, BridgeEvent, BridgeHost, BridgeTopic, BridgeWindow,
    ChosenPathReply, ConfirmDestructiveParams, ConfirmReply, DeviceIdParams, EventSink,
    ExportUpdateParams, MeetingIdParams, OpenUrlParams, Outcome, PageLayoutParams, PermissionKind,
    PermissionKindParams, RecordingState, SaveNotesParams, SelectSpeakerParams,
    SetAutomaticUpdatesParams, SetBoolParams, SetFilterParams, SetQueryParams, SetRetentionParams,
    SetStringParams, SetTabParams, SetTagFilterParams, SetTagsParams, SetTemplateParams,
    SettingsSection, SetupStepParams, SpeakerIdParams, SpeakerOption, SpeakerOptionKind,
    SpeakerOptionsParams, SpeakerOptionsReply, StartRecordingParams, SummariesUpdateParams,
    WindowParams,
};
use steno_core::paths::path_from_file_url;
use steno_core::protocols::SecretKey;
use steno_core::{Store, StoreError};
use uuid::Uuid;

use crate::main_window::progress::MeetingEvent;
use crate::main_window::snapshots::{self as main_snapshots, AppState};
use crate::main_window::{MeetingDetailViewModel, MeetingListViewModel, ProcessingProgressModel};
use crate::onboarding::{self, OnboardingViewModel};
use crate::publisher::{RECORDING_INTERVAL, TopicPublisher};
use crate::services::Services;
use crate::settings::{
    AudioSettingsViewModel, GeneralSettingsViewModel, LlmPreset, LlmSettingsViewModel,
    ObsidianSettingsViewModel, PhonesSettingsViewModel, SpeechSettingsViewModel, llm::Probe,
    overview, snapshots as settings_snapshots, transcription::AssetState,
};
use crate::speakers::{OptionKind, SpeakerOption as ModelOption};
use crate::speech::{ModelAsset, SpeechEngineId};

/// What the shell tells the host once. Swift: the bundle's marketing
/// version and the viewer's calendar.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// `CFBundleShortVersionString`; "0" in a bundle without one.
    pub version: String,
    /// The viewer's zone, for day groups and derived titles.
    pub zone: FixedOffset,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
            version: "0".to_owned(),
            zone: crate::labels::utc(),
        }
    }
}

/// The destructive confirmation: a native alert in the shell, a stub in
/// tests. Swift: `MainWindowBridge.Confirm`.
pub type Confirm = Box<dyn Fn(&ConfirmDestructiveParams) -> bool + Send + Sync>;

/// The folder chooser: takes the folder to start in and returns the
/// choice, `None` when cancelled. Swift: `SettingsBridge.ChooseFolder`.
pub type ChooseFolder = Box<dyn Fn(Option<&Path>) -> Option<PathBuf> + Send + Sync>;

/// What constructing a host can fail on: the store.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The core's boundaries are async; the host waits on them here, on one
/// current-thread runtime shared by every call.
pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime builds without I/O or time")
        })
        .block_on(future)
}

/// The view models and the controller state behind the host's mutex.
struct Inner {
    app: AppState,
    /// Whether the list had meetings at its last publish; the `app` topic
    /// reads this so it republishes when the list empties or fills, not
    /// with every list change.
    has_meetings: bool,
    /// `meetings.select` for a meeting the list does not have yet.
    pending_selection: Option<Uuid>,
    list: MeetingListViewModel,
    detail: Option<MeetingDetailViewModel>,
    progress: ProcessingProgressModel,
    general: GeneralSettingsViewModel,
    audio: AudioSettingsViewModel,
    speech: SpeechSettingsViewModel,
    llm: LlmSettingsViewModel,
    obsidian: ObsidianSettingsViewModel,
    phones: PhonesSettingsViewModel,
    subtitles: BTreeMap<SettingsSection, String>,
    onboarding: OnboardingViewModel,
    publisher: TopicPublisher,
}

/// What every clone of a [`Host`] shares: the store, the services, the
/// view models and the sink.
struct Shared {
    store: Arc<Store>,
    services: Services,
    config: HostConfig,
    inner: Mutex<Inner>,
    sink: Mutex<Option<Arc<dyn EventSink>>>,
    /// Held for the whole of a publish, so snapshots reach the sink in the
    /// order they were built while `inner` is free between the two.
    publishing: Mutex<()>,
    dialogs: RwLock<Dialogs>,
    /// The thread that flushes a throttled topic when its interval is up.
    flusher: thread::Thread,
}

/// The shell's two native surfaces.
struct Dialogs {
    confirm: Confirm,
    choose_folder: ChooseFolder,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // The flush thread holds a `Weak`; woken, it finds no host and ends.
        self.flusher.unpark();
    }
}

/// Swift: `MainWindowBridge`, `SettingsBridge` and `OnboardingBridge` in
/// one, over `AppController`. Cheap to clone: every clone is a handle on
/// the same view models, bound to the window it answers for
/// ([`Host::for_window`]).
///
/// The whole drive, without a shell:
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::sync::Arc;
/// use steno_bridge::{BridgeEvent, BridgeHost, BridgeTopic, BridgeWindow, EventSink};
/// use steno_core::Store;
/// use steno_host::fakes::FakeServices;
/// use steno_host::{Host, HostConfig};
///
/// // Where the snapshots go: the shell's window channels, here a list.
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
/// // `page.ready` publishes every topic once, in order.
/// host.page_ready()?;
/// assert_eq!(sink.0.lock().unwrap().len(), 12);
///
/// // A write from elsewhere (the pipeline, another process): the host
/// // reloads what Swift followed through observation and republishes.
/// let mut settings = store.settings()?;
/// settings.meeting_detection_enabled = false;
/// store.save_settings(&settings)?;
/// host.store_changed();
/// assert_eq!(
///     host.snapshot(BridgeTopic::SettingsGeneral).unwrap()["detectionEnabled"],
///     false
/// );
///
/// // One dispatcher per window answers that window's commands.
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

/// Every topic, in the order `page.ready` publishes them: the main
/// window's, then the Settings sections, then onboarding.
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

const SECTION_TOPICS: [BridgeTopic; 6] = [
    BridgeTopic::SettingsGeneral,
    BridgeTopic::SettingsRecording,
    BridgeTopic::SettingsTranscription,
    BridgeTopic::SettingsSummaries,
    BridgeTopic::SettingsExport,
    BridgeTopic::SettingsPhone,
];

/// Swift: `MainWindowBridge.deleteMeetingMessage`.
pub const DELETE_MEETING_MESSAGE: &str = "The transcript, summary, tasks and the recording on this Mac are removed. Files already exported to Obsidian stay. People stay.";

/// Swift: `MainWindowBridge.deleteRecordingPrompt`.
pub fn delete_recording_prompt() -> ConfirmDestructiveParams {
    ConfirmDestructiveParams {
        title: "Delete this recording now?".to_owned(),
        message: "The recording is deleted shortly. The transcript, summary and exports stay."
            .to_owned(),
        confirm_title: "Delete recording".to_owned(),
    }
}

/// The preferences flag `AppController` set on its first launch.
pub const LOGIN_ITEM_REGISTERED_KEY: &str = "steno.loginItemRegistered";

fn no_such_meeting() -> BridgeError {
    BridgeError::not_found("No meeting with that id is listed.")
}

fn no_selection() -> BridgeError {
    BridgeError::not_found("No meeting is selected.")
}

impl Host {
    /// Loads every view model from the store and the services. Nothing is
    /// published until a sink is attached and `page.ready` arrives.
    pub fn new(
        store: Arc<Store>,
        services: Services,
        config: HostConfig,
    ) -> Result<Self, HostError> {
        let secret = block_on(services.secrets.secret(&SecretKey::llm_api_key()))
            .ok()
            .flatten();
        let mut general = GeneralSettingsViewModel::new(&services);
        general.load(&store, &services);
        let mut audio = AudioSettingsViewModel::new();
        audio.load(&store, &services);
        let mut speech = SpeechSettingsViewModel::new();
        speech.load(&store, &services);
        let mut llm = LlmSettingsViewModel::new();
        llm.load(&store, &services, secret.clone());
        let mut obsidian = ObsidianSettingsViewModel::default();
        obsidian.load(&store);
        let mut phones = PhonesSettingsViewModel::new(&services);
        phones.load(&services);
        phones.refresh(&services);
        let subtitles = overview::refresh(&store, &services, &config.version);
        let mut onboarding = OnboardingViewModel::new();
        onboarding.load(&store, &services, secret);
        let mut list = MeetingListViewModel::new(config.zone);
        list.reload(&store);
        let mut progress = ProcessingProgressModel::default();
        progress.meetings_changed(&list.all, services.clock.now());
        let app = AppState {
            stored_settings: store.settings().ok(),
            phone: Self::phone_card(&services),
            ..AppState::default()
        };
        let publisher = TopicPublisher::new(
            TOPICS.to_vec(),
            BTreeMap::from([(BridgeTopic::Recording, RECORDING_INTERVAL)]),
        );
        // The flush thread gets its handle on the host once the host exists.
        let (handle, receiver) = mpsc::channel::<Weak<Shared>>();
        let flusher = thread::Builder::new()
            .name("steno-host-flush".to_owned())
            .spawn(move || flush_loop(&receiver))
            .map_err(StoreError::Io)?;
        let shared = Arc::new(Shared {
            store,
            services,
            config,
            inner: Mutex::new(Inner {
                app,
                has_meetings: false,
                pending_selection: None,
                list,
                detail: None,
                progress,
                general,
                audio,
                speech,
                llm,
                obsidian,
                phones,
                subtitles,
                onboarding,
                publisher,
            }),
            sink: Mutex::new(None),
            publishing: Mutex::new(()),
            dialogs: RwLock::new(Dialogs {
                confirm: Box::new(|_| false),
                choose_folder: Box::new(|_| None),
            }),
            flusher: flusher.thread().clone(),
        });
        // The thread outlives this send only while a host exists.
        let _ = handle.send(Arc::downgrade(&shared));
        Ok(Host {
            shared,
            window: BridgeWindow::Main,
        })
    }

    /// The shell's two native surfaces, shared by every clone. Without
    /// them every destructive prompt declines and every chooser cancels.
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

    /// Asks the shell's destructive prompt.
    fn confirm(&self, prompt: &ConfirmDestructiveParams) -> bool {
        (self.dialogs().confirm)(prompt)
    }

    /// The same host, answering for `window`: see the module doc.
    #[must_use]
    pub fn for_window(&self, window: BridgeWindow) -> Host {
        Host {
            shared: self.shared.clone(),
            window,
        }
    }

    /// The window this handle answers for.
    #[must_use]
    pub fn window(&self) -> BridgeWindow {
        self.window
    }

    /// The sink snapshots go to. Swift: `attach(_:)`.
    pub fn attach(&self, sink: Arc<dyn EventSink>) {
        *lock(&self.shared.sink) = Some(sink);
    }

    #[must_use]
    pub fn services(&self) -> &Services {
        &self.shared.services
    }

    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.shared.store
    }

    /// The iPhone card for the `app` topic: the first paired device, as
    /// reachable as the listener is listening. The Swift main window left
    /// it nil; the parity list notes the difference.
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

    // Publishing

    /// The topic's snapshot as the view models stand, with the publish
    /// hooks the Swift bridges ran before building (the list's fill and
    /// the detail swap). `None` for a topic the host does not publish;
    /// `null` for `meeting.detail` without a selection.
    pub fn snapshot(&self, topic: BridgeTopic) -> Option<Value> {
        let mut inner = self.lock();
        self.build(&mut inner, topic)
    }

    fn build(&self, inner: &mut Inner, topic: BridgeTopic) -> Option<Value> {
        let now = self.now();
        let snapshot = match topic {
            BridgeTopic::App => to_value(main_snapshots::app_snapshot(
                &inner.app,
                inner.has_meetings,
                &self.shared.config.version,
            )),
            BridgeTopic::Recording => to_value(main_snapshots::recording_snapshot(
                &self.shared.services.recorder.status(),
            )),
            BridgeTopic::Progress => to_value(main_snapshots::progress_snapshot(&inner.progress)),
            BridgeTopic::MeetingsList => {
                Self::follow_list_fill(inner);
                self.sync_detail(inner);
                to_value(main_snapshots::list_snapshot(&inner.list, now))
            }
            BridgeTopic::MeetingDetail => {
                self.sync_detail(inner);
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
            )),
            BridgeTopic::SettingsRecording => to_value(settings_snapshots::recording(
                &inner.audio,
                inner.subtitle(SettingsSection::Recording),
            )),
            BridgeTopic::SettingsTranscription => to_value(settings_snapshots::transcription(
                &inner.speech,
                inner.subtitle(SettingsSection::Transcription),
            )),
            BridgeTopic::SettingsSummaries => to_value(settings_snapshots::summaries(
                &inner.llm,
                inner.subtitle(SettingsSection::Summaries),
            )),
            BridgeTopic::SettingsExport => to_value(settings_snapshots::export(
                &inner.obsidian,
                inner.subtitle(SettingsSection::Export),
            )),
            BridgeTopic::SettingsPhone => to_value(settings_snapshots::phone(
                &inner.phones,
                &self.shared.services,
                inner.subtitle(SettingsSection::Phone),
            )),
            BridgeTopic::Onboarding => to_value(onboarding::snapshot(&inner.onboarding)),
        };
        snapshot.ok()
    }

    /// Flushes every pending topic whose interval is up: builds the
    /// snapshots under the lock, emits them once it is released and the
    /// page is ready, in build order, and runs the publish's side effects
    /// (a deep link is consumed by the publish that carried it). Called at
    /// the end of every command and by the flush thread; a throttled topic
    /// left pending arms that thread.
    fn publish(&self) {
        let _publishing = lock(&self.shared.publishing);
        // Bounded: each pass only schedules what a consumed request changed.
        for _ in 0..4 {
            let (sink, batch) = {
                let mut inner = self.lock();
                let due = inner.publisher.take_due(Instant::now());
                if due.is_empty() {
                    break;
                }
                // Nothing goes out before `page.ready`; the snapshots are
                // still built for their side effects.
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
                (sink, batch)
            };
            if let Some(sink) = sink {
                for event in batch {
                    sink.emit(event);
                }
            }
        }
        if self.next_flush_due().is_some() {
            self.shared.flusher.unpark();
        }
    }

    /// The app publish consumes the controller's requests after the
    /// snapshot has carried them once: the meeting becomes the selection,
    /// the section is cleared once the page saw it.
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

    /// The list publish's side effects: a selection asked for ahead of its
    /// row is applied once the row is listed, the first fill selects the
    /// newest meeting, and `has_meetings` flips (re-publishing `app`) only
    /// when the list empties or fills.
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

    /// One detail model per selected meeting, torn down when the selection
    /// moves on.
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
                &self.shared.store,
                &*self.shared.services.file_system,
                &*self.shared.services.pipeline,
            );
            inner.detail = Some(detail);
        }
        inner.publisher.schedule(BridgeTopic::MeetingDetail);
    }

    /// How long until a throttled topic may go out; `None` when nothing
    /// waits on its interval. The flush thread's wake-up; a test reads it to
    /// know a `recording` change is on its way.
    #[must_use]
    pub fn next_flush_due(&self) -> Option<Duration> {
        self.lock().publisher.next_due(Instant::now())
    }

    // What the app tells the host between commands

    /// The store changed (the pipeline wrote, a recording started, another
    /// process saved): reload what Swift followed through observation and
    /// republish it. When the stored settings changed, the Settings
    /// sections load again too (an unsaved draft in one of them is replaced
    /// by the stored values, as reopening the Swift window did); a write
    /// that left the settings alone keeps every draft.
    pub fn store_changed(&self) {
        {
            let mut guard = self.lock();
            let inner = &mut *guard;
            let now = self.now();
            inner.list.reload(&self.shared.store);
            inner.progress.meetings_changed(&inner.list.all, now);
            if let Some(detail) = inner.detail.as_mut() {
                detail.reload(
                    &self.shared.store,
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
            self.follow_settings(inner, true);
        }
        self.publish();
    }

    /// The recorder's state, levels or messages changed.
    pub fn recorder_changed(&self) {
        self.lock().publisher.schedule(BridgeTopic::Recording);
        self.publish();
    }

    /// One event from the pipeline's bus.
    pub fn apply_meeting_event(&self, event: &MeetingEvent) {
        {
            let mut inner = self.lock();
            let now = self.now();
            inner.progress.apply(event, now);
            inner.publisher.schedule(BridgeTopic::Progress);
        }
        self.publish();
    }

    /// The handover listener or a transfer changed.
    pub fn phones_changed(&self) {
        {
            let mut inner = self.lock();
            inner.phones.refresh(&self.shared.services);
            inner.publisher.schedule(BridgeTopic::SettingsPhone);
            self.refresh_subtitles(&mut inner);
        }
        self.publish();
    }

    /// The pairing poll, which the shell drives: every two seconds while
    /// `settings.iphone` shows a code (Swift's `pairingPoll`), a phone that
    /// arrived closes the code and a code that ran out closes itself. The
    /// host has no timer for this; the flush thread covers the publisher
    /// only.
    pub fn refresh_pairing(&self) {
        {
            let mut inner = self.lock();
            let now = self.now();
            inner
                .phones
                .refresh_after_pairing(&self.shared.services, now);
            inner.publisher.schedule(BridgeTopic::SettingsPhone);
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

    /// Deep link into Settings. Swift: `AppController.openSettings`.
    pub fn open_settings(&self, section: SettingsSection) {
        {
            let mut inner = self.lock();
            inner.app.requested_settings_section = Some(section);
            inner.publisher.schedule(BridgeTopic::App);
        }
        self.publish();
    }

    /// Whether the onboarding window should open at launch.
    #[must_use]
    pub fn should_open_onboarding(&self) -> bool {
        OnboardingViewModel::should_open(&self.shared.store, &self.shared.services)
    }

    /// The first launch registers the login item when the setting says so.
    /// Swift: `AppController.registerLoginItemOnFirstLaunch`.
    pub fn register_login_item_on_first_launch(&self) {
        let preferences = &self.shared.services.preferences;
        if preferences.flag(LOGIN_ITEM_REGISTERED_KEY) {
            return;
        }
        let Ok(settings) = self.shared.store.settings() else {
            return;
        };
        if !settings.launch_at_login {
            return;
        }
        preferences.set_flag(LOGIN_ITEM_REGISTERED_KEY, true);
        if self.shared.services.login_item.status()
            == crate::services::LoginItemStatus::NotRegistered
        {
            let _ = self.shared.services.login_item.set_enabled(true);
        }
        let mut inner = self.lock();
        inner.general.login_item = self.shared.services.login_item.status();
    }

    /// The user closed the onboarding window with its own close button:
    /// that counts as having seen the pages, so the opener returns only for
    /// a missing required permission. Swift: `OnboardingWindow.onDisappear`.
    pub fn onboarding_window_closed(&self) {
        OnboardingViewModel::mark_completed(&self.shared.services);
    }

    // Helpers for the commands

    fn with_detail<R>(
        inner: &mut Inner,
        body: impl FnOnce(&mut MeetingDetailViewModel) -> R,
    ) -> Outcome<R> {
        inner.detail.as_mut().map(body).ok_or_else(no_selection)
    }

    /// A command on the selected meeting's detail (`no_selection` without
    /// one) that changes only the detail's own state: the detail topic
    /// republishes.
    fn detail_command<R>(&self, body: impl FnOnce(&mut MeetingDetailViewModel) -> R) -> Outcome<R> {
        self.on_detail(body, |inner| {
            inner.publisher.schedule(BridgeTopic::MeetingDetail);
        })
    }

    /// A command on the selected meeting's detail after which the detail,
    /// or the pipeline on its behalf, wrote to the store: the detail and
    /// the list reload before publishing.
    fn detail_write<R>(&self, body: impl FnOnce(&mut MeetingDetailViewModel) -> R) -> Outcome<R> {
        self.on_detail(body, |inner| self.reload_detail(inner))
    }

    fn on_detail<R>(
        &self,
        body: impl FnOnce(&mut MeetingDetailViewModel) -> R,
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

    /// Runs the shell's folder chooser from `current` and `apply` on the
    /// chosen folder; the reply carries the choice, `None` when cancelled.
    fn choose(&self, current: Option<&Path>, apply: impl FnOnce(&Path)) -> ChosenPathReply {
        let chosen = (self.dialogs().choose_folder)(current);
        if let Some(folder) = &chosen {
            apply(folder);
        }
        ChosenPathReply {
            path: chosen.map(|path| path.to_string_lossy().into_owned()),
        }
    }

    /// The detail's store state after a write the detail made itself.
    fn reload_detail(&self, inner: &mut Inner) {
        if let Some(detail) = inner.detail.as_mut() {
            detail.reload(
                &self.shared.store,
                &*self.shared.services.file_system,
                &*self.shared.services.pipeline,
            );
        }
        inner.list.reload(&self.shared.store);
        inner.publisher.schedule(BridgeTopic::MeetingsList);
        inner.publisher.schedule(BridgeTopic::MeetingDetail);
    }

    /// The sidebar subtitles follow every Settings command; a change
    /// republishes every section, as the observed map did.
    fn refresh_subtitles(&self, inner: &mut Inner) {
        let next = overview::refresh(
            &self.shared.store,
            &self.shared.services,
            &self.shared.config.version,
        );
        if next != inner.subtitles {
            inner.subtitles = next;
            for topic in SECTION_TOPICS {
                inner.publisher.schedule(topic);
            }
        }
    }

    /// After a command that may have written settings: the stored settings
    /// the `app` topic and the detail read are re-read, and when they
    /// changed from another window's point of view (`reload_sections`: a
    /// store change, an onboarding save) the Settings sections load again,
    /// as Swift's did when its window opened. The subtitles follow either
    /// way.
    fn follow_settings(&self, inner: &mut Inner, reload_sections: bool) {
        let latest = self.shared.store.settings().ok();
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
        self.refresh_subtitles(inner);
    }

    /// Every Settings section's `load`, as `SettingsBridge.load()` ran them
    /// when the window opened. A download in flight keeps its state.
    fn reload_sections(&self, inner: &mut Inner) {
        let store = &self.shared.store;
        let services = &self.shared.services;
        let secret = block_on(services.secrets.secret(&SecretKey::llm_api_key()))
            .ok()
            .flatten();
        inner.general.load(store, services);
        inner.audio.load(store, services);
        inner.speech.load(store, services);
        inner.llm.load(store, services, secret);
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

    /// An onboarding command: page 2 writes the same settings the Summaries
    /// and Export sections show, so those reload when it did. The exit is
    /// the model's: the command that sets `finished` closes the window, as
    /// `OnboardingWindow` did on that change, after the page saw it.
    fn onboarding_command(&self, body: impl FnOnce(&mut Inner)) {
        let finished = {
            let mut inner = self.lock();
            let was_finished = inner.onboarding.finished;
            body(&mut inner);
            inner.publisher.schedule(BridgeTopic::Onboarding);
            self.follow_settings(&mut inner, true);
            !was_finished && inner.onboarding.finished
        };
        self.publish();
        self.run_pending_probe(BridgeWindow::Onboarding);
        if finished {
            self.shared
                .services
                .opener
                .close_window(BridgeWindow::Onboarding);
        }
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

    /// Marks a view model busy under the lock and publishes, runs `work`
    /// with the lock released, then applies its result under the lock and
    /// publishes again: the page sees the busy flag while a prompt or a
    /// probe is up, and no other topic waits on it. `begin` returns `None`
    /// when there is nothing to run. Swift awaited inside the model, on the
    /// main actor, which blocked nothing else.
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

    /// The topic the window's Summaries form rides on.
    fn summaries_topic(window: BridgeWindow) -> BridgeTopic {
        if window == BridgeWindow::Onboarding {
            BridgeTopic::Onboarding
        } else {
            BridgeTopic::SettingsSummaries
        }
    }

    /// A command on `window`'s Summaries form: that window's model, its own
    /// topic, and the probe the model asked for run outside the lock
    /// afterwards. The four form commands pass the calling window (see the
    /// module doc), the Codex card and Save the Settings window.
    fn summaries_command(
        &self,
        window: BridgeWindow,
        body: impl FnOnce(&mut LlmSettingsViewModel),
    ) {
        {
            let mut inner = self.lock();
            body(inner.llm_mut(window));
            inner.publisher.schedule(Self::summaries_topic(window));
            self.follow_settings(&mut inner, window == BridgeWindow::Onboarding);
        }
        self.publish();
        self.run_pending_probe(window);
    }

    /// Runs the probe the window's Summaries model asked for, if any:
    /// `isTesting` goes out first, the service is called with the lock
    /// released, the report lands and goes out.
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
                self.command(&[BridgeTopic::SettingsTranscription], |inner| {
                    inner.speech.asset_states.insert(
                        asset,
                        AssetState::Downloading {
                            fraction,
                            phase: phase.to_owned(),
                        },
                    );
                });
            });
        self.settings_command(BridgeTopic::SettingsTranscription, |inner| {
            let state = match outcome {
                Ok(()) => AssetState::Installed {
                    bytes: self.shared.services.speech_models.installed_size(asset),
                },
                Err(error) => AssetState::Failed(error.to_string()),
            };
            inner.speech.asset_states.insert(asset, state);
        });
    }

    /// Applies the keep flag, after the delete-recording prompt when
    /// `confirming`; the reply says whether the user went ahead.
    fn set_keep_audio(&self, keep: bool, confirming: bool) -> Outcome<ConfirmReply> {
        if confirming && !self.confirm(&delete_recording_prompt()) {
            return Ok(ConfirmReply { confirmed: false });
        }
        self.detail_write(|detail| {
            detail.set_keep_audio(keep, &self.shared.store, &*self.shared.services.pipeline);
        })
        .map(|()| ConfirmReply { confirmed: true })
    }

    /// Page 1 moves on by itself once every step is handled, as the Swift
    /// page did; only after a command that can change a step, so Back from
    /// page 2 stays on page 1.
    fn advance_if_handled(&self, inner: &mut Inner) {
        if inner.onboarding.page == steno_bridge::OnboardingPage::Permissions
            && inner.onboarding.permissions_handled()
        {
            inner.onboarding.advance(&self.shared.services);
        }
    }

    /// The page's option back into the model's: a person is looked up
    /// among the people the picker was built from; a create row carries
    /// its name.
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

    /// The Summaries model the window owns: onboarding's on page 2, the
    /// Settings section's otherwise.
    fn llm_mut(&mut self, window: BridgeWindow) -> &mut LlmSettingsViewModel {
        if window == BridgeWindow::Onboarding {
            &mut self.onboarding.llm
        } else {
            &mut self.llm
        }
    }
}

/// The flush thread: wakes when the host arms it or a throttled topic's
/// interval is up, publishes what is due, and ends once no host is left.
fn flush_loop(receiver: &mpsc::Receiver<Weak<Shared>>) {
    let Ok(shared) = receiver.recv() else {
        return;
    };
    loop {
        let Some(strong) = shared.upgrade() else {
            return;
        };
        let wait = lock(&strong.inner).publisher.next_due(Instant::now());
        match wait {
            Some(wait) if wait.is_zero() => Host {
                shared: strong,
                window: BridgeWindow::Main,
            }
            .publish(),
            Some(wait) => {
                drop(strong);
                thread::park_timeout(wait);
            }
            None => {
                drop(strong);
                thread::park();
            }
        }
    }
}

/// A picker row in the wire vocabulary; the tag is the row's detail.
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

#[allow(clippy::too_many_lines)]
impl BridgeHost for Host {
    fn page_ready(&self) -> Outcome<()> {
        self.lock().publisher.page_did_become_ready();
        self.publish();
        Ok(())
    }

    fn page_layout(&self, _params: PageLayoutParams) -> Outcome<()> {
        // Validated and dropped: nothing reads the page's size yet.
        Ok(())
    }

    // meetings

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
            inner.list.set_query(params.query, &self.shared.store);
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
        let prompt = {
            let inner = self.lock();
            let meeting = inner
                .list
                .all
                .iter()
                .find(|meeting| meeting.id == params.meeting_id)
                .ok_or_else(no_such_meeting)?;
            if !MeetingListViewModel::can_delete(meeting) {
                let message = if meeting.state.kind() == steno_core::MeetingStateKind::Recording {
                    "This meeting is still recording."
                } else {
                    "This meeting is still being processed."
                };
                return Err(BridgeError::failed(message));
            }
            ConfirmDestructiveParams {
                title: format!(
                    "Delete “{}”?",
                    crate::labels::display_title(meeting, self.now(), self.shared.config.zone)
                ),
                message: DELETE_MEETING_MESSAGE.to_owned(),
                confirm_title: "Delete".to_owned(),
            }
        };
        let confirmed = self.confirm(&prompt);
        if confirmed {
            let now = self.now();
            self.command(
                &[BridgeTopic::MeetingsList, BridgeTopic::Progress],
                |inner| {
                    inner.list.delete(
                        params.meeting_id,
                        &self.shared.store,
                        &*self.shared.services.file_system,
                    );
                    inner.list.reload(&self.shared.store);
                    // The store's `deleted` event: a queued meeting's
                    // progress entry goes with it.
                    inner.progress.apply(
                        &MeetingEvent::Deleted {
                            meeting_id: params.meeting_id,
                        },
                        now,
                    );
                },
            );
        }
        Ok(ConfirmReply { confirmed })
    }

    // meeting

    fn meeting_set_tab(&self, params: SetTabParams) -> Outcome<()> {
        self.detail_command(|detail| detail.tab = params.tab)
    }

    fn meeting_set_tags(&self, params: SetTagsParams) -> Outcome<()> {
        let tags = MeetingDetailViewModel::tags_from(&params.tags);
        let now = self.now();
        self.detail_write(|detail| detail.set_tags(tags, &self.shared.store, now))
    }

    fn meeting_set_template(&self, params: SetTemplateParams) -> Outcome<()> {
        let now = self.now();
        self.detail_write(|detail| {
            detail.set_template(
                &params.template_id,
                &self.shared.store,
                now,
                &*self.shared.services.pipeline,
            );
        })
    }

    fn meeting_rerun_summary(&self) -> Outcome<()> {
        self.detail_write(|detail| detail.rerun_summary(&*self.shared.services.pipeline))
    }

    fn meeting_reexport(&self) -> Outcome<()> {
        self.detail_write(|detail| detail.reexport(&*self.shared.services.pipeline))
    }

    fn meeting_set_keep_audio(&self, params: SetBoolParams) -> Outcome<ConfirmReply> {
        let confirming = {
            let inner = self.lock();
            let detail = inner.detail.as_ref().ok_or_else(no_selection)?;
            !params.value && detail.would_delete_now()
        };
        self.set_keep_audio(params.value, confirming)
    }

    fn meeting_delete_recording_now(&self) -> Outcome<ConfirmReply> {
        if self.lock().detail.is_none() {
            return Err(no_selection());
        }
        self.set_keep_audio(false, true)
    }

    fn meeting_save_notes(&self, params: SaveNotesParams) -> Outcome<()> {
        // The page debounces typing and names the meeting, so the text is
        // written where it says, selected or not, the moment it arrives.
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
            .and_then(|asset| path_from_file_url(&asset.url))
            .filter(|_| detail.recording_files_exist)
            .ok_or_else(|| BridgeError::not_found("The recording is no longer on this Mac."))?;
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

    // speakers

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
            let outcome = Self::with_detail(&mut inner, |detail| {
                if detail
                    .speakers
                    .select(&option, params.speaker_id, &self.shared.store, now)
                {
                    detail.speakers_changed();
                }
                // The page has no "picker closed" moment the host can see;
                // one re-export per change is idempotent.
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

    // recording

    fn recording_start(&self, params: StartRecordingParams) -> Outcome<()> {
        // The sidebar control's start: the recorder starts, then the live
        // row is requested so the window selects it. A start that fails
        // re-reads the permissions.
        self.shared.services.recorder.start(params.mode, None);
        let status = self.shared.services.recorder.status();
        self.command(
            &[BridgeTopic::Recording, BridgeTopic::App],
            |inner| match status.state {
                RecordingState::Recording => inner.app.requested_meeting_id = status.meeting_id,
                RecordingState::Idle => self.shared.services.recorder.refresh_permissions(),
                RecordingState::Starting | RecordingState::Stopping => {}
            },
        );
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

    fn setup_dismiss_banner(&self) -> Outcome<()> {
        self.command(&[BridgeTopic::App], |inner| {
            inner.app.setup_banner_dismissed = true;
        });
        Ok(())
    }

    // settings.general

    fn settings_general_set_launch_at_login(&self, params: SetBoolParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsGeneral, |inner| {
            inner.general.set_launch_at_login(
                params.value,
                &self.shared.store,
                &self.shared.services,
            );
        });
        Ok(())
    }

    fn settings_general_set_detection(&self, params: SetBoolParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsGeneral, |inner| {
            inner
                .general
                .set_detection_enabled(params.value, &self.shared.store);
        });
        Ok(())
    }

    fn settings_general_set_default_template(&self, params: SetTemplateParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsGeneral, |inner| {
            inner
                .general
                .set_default_template(&params.template_id, &self.shared.store);
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

    // settings.recording

    fn settings_recording_set_input_device(&self, params: SetStringParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsRecording, |inner| {
            let uid = (!params.value.is_empty()).then_some(params.value);
            inner.audio.set_input_device(uid, &self.shared.store);
        });
        Ok(())
    }

    fn settings_recording_refresh_devices(&self) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsRecording, |inner| {
            inner.audio.refresh_devices(&self.shared.services);
        });
        Ok(())
    }

    fn settings_recording_choose_folder(&self) -> Outcome<ChosenPathReply> {
        let current = self.lock().audio.audio_folder.clone();
        Ok(self.choose(Some(&current), |folder| {
            self.settings_command(BridgeTopic::SettingsRecording, |inner| {
                inner
                    .audio
                    .set_audio_folder(folder, &self.shared.store, &self.shared.services);
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
                &self.shared.store,
                &self.shared.services,
            );
        });
        Ok(())
    }

    fn settings_recording_request_permission(&self, params: PermissionKindParams) -> Outcome<()> {
        self.outside_lock(
            BridgeTopic::SettingsRecording,
            |inner| inner.audio.begin_request(params.kind).then_some(()),
            |()| self.shared.services.permissions.request(params.kind),
            |inner, state| inner.audio.finish_request(params.kind, state),
        );
        Ok(())
    }

    // settings.transcription

    fn settings_transcription_set_engine(&self, params: SetStringParams) -> Outcome<()> {
        let engine: SpeechEngineId = params.value.parse().map_err(|_| {
            BridgeError::invalid_params(format!("Unknown speech engine {}.", params.value))
        })?;
        self.settings_command(BridgeTopic::SettingsTranscription, |inner| {
            inner
                .speech
                .set_engine(engine, &self.shared.store, &self.shared.services);
        });
        Ok(())
    }

    fn settings_transcription_download(&self, params: AssetIdParams) -> Outcome<()> {
        let asset = parse_asset(&params)?;
        // The reply returns at once; the download runs on its own thread
        // and publishes as it goes, as Swift's task did.
        {
            let mut inner = self.lock();
            if matches!(inner.speech.state_of(asset), AssetState::Downloading { .. }) {
                return Ok(());
            }
            inner.speech.asset_states.insert(
                asset,
                AssetState::Downloading {
                    fraction: 0.0,
                    phase: "starting".to_owned(),
                },
            );
            inner.publisher.schedule(BridgeTopic::SettingsTranscription);
        }
        self.publish();
        let host = self.clone();
        thread::Builder::new()
            .name(format!("steno-download-{}", asset.as_str()))
            .spawn(move || host.run_download(asset))
            .map_err(|error| {
                BridgeError::failed(format!("The download could not start: {error}"))
            })?;
        Ok(())
    }

    fn settings_transcription_remove(&self, params: AssetIdParams) -> Outcome<()> {
        let asset = parse_asset(&params)?;
        self.settings_command(BridgeTopic::SettingsTranscription, |inner| {
            inner.speech.remove(asset, &self.shared.services);
        });
        Ok(())
    }

    // settings.summaries: the four form commands answer on the calling
    // window's model (see the module doc), the rest on the Settings section.

    fn settings_summaries_select_preset(&self, params: SetStringParams) -> Outcome<()> {
        let preset: LlmPreset = params.value.parse().map_err(|_| {
            BridgeError::invalid_params(format!("Unknown summaries service {}.", params.value))
        })?;
        let now = self.now();
        self.summaries_command(self.window, |llm| {
            llm.select_preset(preset, &self.shared.store, &self.shared.services, now);
        });
        Ok(())
    }

    fn settings_summaries_update(&self, params: SummariesUpdateParams) -> Outcome<()> {
        self.summaries_command(self.window, |llm| llm.apply_update(&params));
        Ok(())
    }

    fn settings_summaries_save(&self) -> Outcome<()> {
        let now = self.now();
        self.summaries_command(BridgeWindow::Settings, |llm| {
            llm.commit(&self.shared.store, &self.shared.services, now);
        });
        Ok(())
    }

    fn settings_summaries_test(&self) -> Outcome<()> {
        self.summaries_command(self.window, LlmSettingsViewModel::request_probe);
        Ok(())
    }

    fn settings_summaries_confirm_codex(&self) -> Outcome<()> {
        let now = self.now();
        self.summaries_command(BridgeWindow::Settings, |llm| {
            llm.confirm_codex(&self.shared.store, &self.shared.services, now);
        });
        Ok(())
    }

    fn settings_summaries_refresh_codex_status(&self) -> Outcome<()> {
        // A file read, outside the lock all the same.
        let account = self.shared.services.llm.codex_account();
        self.summaries_command(self.window, |llm| llm.apply_codex_account(account));
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
        self.summaries_command(BridgeWindow::Settings, |llm| {
            llm.select_codex_model(
                &params.value,
                &self.shared.store,
                &self.shared.services,
                now,
            );
        });
        Ok(())
    }

    fn settings_summaries_stop_using_codex(&self) -> Outcome<()> {
        let now = self.now();
        self.summaries_command(BridgeWindow::Settings, |llm| {
            llm.stop_using_codex(&self.shared.store, &self.shared.services, now);
        });
        Ok(())
    }

    // settings.export

    fn settings_export_set_enabled(&self, params: SetBoolParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsExport, |inner| {
            inner
                .obsidian
                .set_enabled(params.value, &self.shared.store, &self.shared.services);
        });
        Ok(())
    }

    fn settings_export_choose_vault(&self) -> Outcome<ChosenPathReply> {
        let current = self.lock().obsidian.vault_url();
        Ok(self.choose(current.as_deref(), |folder| {
            self.settings_command(BridgeTopic::SettingsExport, |inner| {
                inner
                    .obsidian
                    .choose_vault(folder, &self.shared.store, &self.shared.services);
            });
        }))
    }

    fn settings_export_update(&self, params: ExportUpdateParams) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsExport, |inner| {
            if let Some(people) = params.people_folder {
                inner.obsidian.people_folder = people;
            }
            if let Some(tag) = params.task_tag {
                inner.obsidian.task_tag = tag;
            }
            if let Some(include) = params.include_audio {
                inner.obsidian.set_include_audio(
                    include,
                    &self.shared.store,
                    &self.shared.services,
                );
            }
        });
        Ok(())
    }

    fn settings_export_save(&self) -> Outcome<()> {
        self.settings_command(BridgeTopic::SettingsExport, |inner| {
            inner
                .obsidian
                .commit(&self.shared.store, &self.shared.services);
        });
        Ok(())
    }

    // settings.iphone

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

    // onboarding

    fn onboarding_request(&self, params: PermissionKindParams) -> Outcome<()> {
        // Three steps so the page sees `isRequesting` while the prompt is
        // up, as it did with Swift's awaited request.
        self.onboarding_command(|inner| inner.onboarding.begin_request(params.kind));
        let state = self.shared.services.permissions.request(params.kind);
        self.onboarding_command(|inner| {
            inner.onboarding.finish_request(params.kind, state);
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
            inner
                .onboarding
                .load(&self.shared.store, &self.shared.services, None);
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
                .save_summaries(&self.shared.store, &self.shared.services, now);
        });
        Ok(())
    }

    fn onboarding_confirm_summaries_with_codex(&self) -> Outcome<()> {
        let now = self.now();
        self.onboarding_command(|inner| {
            inner.onboarding.confirm_summaries_with_codex(
                &self.shared.store,
                &self.shared.services,
                now,
            );
        });
        Ok(())
    }

    fn onboarding_choose_vault(&self) -> Outcome<ChosenPathReply> {
        let current = self.lock().onboarding.obsidian.vault_url();
        Ok(self.choose(current.as_deref(), |folder| {
            self.onboarding_command(|inner| {
                inner
                    .onboarding
                    .choose_vault(folder, &self.shared.store, &self.shared.services);
            });
        }))
    }

    fn onboarding_save_vault(&self) -> Outcome<()> {
        self.onboarding_command(|inner| {
            inner
                .onboarding
                .save_vault(&self.shared.store, &self.shared.services);
        });
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

    // system

    fn updates_check(&self) -> Outcome<()> {
        self.shared.services.updater.check_for_updates();
        self.settings_command(BridgeTopic::SettingsGeneral, |_| {});
        Ok(())
    }

    fn system_open_url(&self, params: OpenUrlParams) -> Outcome<()> {
        // Only `https:` and `mailto:`, as `BridgeSystemCommands.openURL`
        // accepted: a page cannot open files, plain `http:` or run scripts.
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
