//! The host side of a call, after `apps/macos/Steno/Web/BridgeDispatcher.swift`
//! and `BridgeRequestParams.swift`: a method and its params in, the typed
//! handler's outcome out. [`BridgeHost`] has one typed method per
//! [`BridgeMethod`]; the [`Dispatcher`] reads the params as the method's
//! contract type, calls the host and wraps the outcome. Two entry points for
//! the two transports: [`Dispatcher::call`] takes a method and its params,
//! which is what the Tauri `bridge_call` command receives (the Tauri
//! transport, `apps/macos/web/src/bridge/tauri-transport.ts`, sends
//! `{ method, params }` and no id);
//! [`Dispatcher::dispatch`] takes the request envelope with its `id` and
//! always answers with a reply envelope, never a rejection, as
//! `webkit-transport.ts` expects.
//!
//! The params reader is more lenient than Swift's in one place, in the
//! direction of accepting what the page never sends: serde reads a params
//! struct from a positional array as well as from an object (`["abc"]` for
//! `{ "meetingId": "abc" }`), where Swift's keyed decoder rejects the array.
//! Not pinned either way. The UUID codec goes the other way and is strict:
//! `steno_core::json::parse_uuid` reads the hyphenated form only, as
//! `UUID(uuidString:)` does; ids on the wire come from the host's own
//! snapshots.
//!
//! Hosts run blocking. Every method takes `&self` and returns when the work
//! is done, including a method that waits on a dialog; the host uses interior
//! mutability for its view models, which is what the Swift `@MainActor` host
//! amounts to. The Tauri shell runs the dispatcher on a blocking thread (the
//! command handler is `async` and awaits a `spawn_blocking`) and bounces
//! anything that needs the UI, such as `ui.confirmDestructive` and the folder
//! panels, to the main thread from inside the host method.
//!
//! Events and replies are not ordered with respect to each other. A host
//! method that publishes a snapshot and then returns sends the snapshot
//! through the [`EventSink`] (the window's event channel) and the reply
//! through the command's future, two paths the `spawn_blocking` hop can
//! reorder; the page must not read a reply as "the snapshots this call caused
//! have arrived", nor the reverse. The Swift host has the same property
//! (`evaluateJavaScript` for events, the message handler's reply for
//! results). The convention, settled in `steno_host::host`: every command
//! publishes the topics it changed before it returns, and every snapshot is
//! a full state, so arrival order only decides which full state the page
//! shows last.
//!
//! [`BridgeHost`] and [`EventSink`] are separate traits because they have
//! different owners: the shell implements the sink (it owns the windows the
//! events go to) and hands it to the host, which only holds it, as an
//! `Arc<dyn EventSink>`. That trait object needs `EventSink` object safe, so
//! `emit` takes a built [`BridgeEvent`] and the generic `publish` lives on
//! the [`EventSinkExt`] extension. A host behind an `Arc` is a host too
//! ([`BridgeHost`] is implemented for `Arc<T: BridgeHost + ?Sized>`), so the
//! shell can share one host between the dispatcher and the event publisher.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::commands::{
    AssetIdParams, ChosenPathReply, ConfirmDestructiveParams, ConfirmReply, DeviceIdParams,
    ExportUpdateParams, MeetingIdParams, OpenUrlParams, PageLayoutParams, PermissionKindParams,
    SaveNotesParams, SelectSpeakerParams, SetAutomaticUpdatesParams, SetBoolParams,
    SetFilterParams, SetQueryParams, SetRetentionParams, SetStringParams, SetTabParams,
    SetTagFilterParams, SetTagsParams, SetTemplateParams, SetupStepParams, SpeakerIdParams,
    SpeakerOptionsParams, SpeakerOptionsReply, StartRecordingParams, SummariesUpdateParams,
    WindowParams,
};
use crate::envelope::{BridgeError, BridgeEvent, BridgeMethod, BridgeReply, BridgeRequest};
use crate::json;
use crate::snapshots::Snapshot;

/// Where a host publishes snapshots. Swift: `BridgeEventSink`.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: BridgeEvent);
}

/// The typed publish path over any [`EventSink`]; an untyped payload goes
/// through [`BridgeEvent::new`] and `emit`.
pub trait EventSinkExt: EventSink {
    /// Publishes a snapshot on its own topic.
    fn publish<S: Snapshot + ?Sized>(&self, snapshot: &S) -> Result<(), serde_json::Error> {
        self.emit(BridgeEvent::snapshot(S::TOPIC, snapshot)?);
        Ok(())
    }
}

impl<T: EventSink + ?Sized> EventSinkExt for T {}

/// A method's outcome as the dispatcher wraps it: a value, nothing, or a
/// contract error.
pub type Outcome<T> = Result<T, BridgeError>;

/// How a method's reply lands in the envelope: nothing for `()`, the JSON
/// value for a typed reply. Swift: `BridgeReplies.value`.
trait IntoResult {
    fn into_result(self) -> Outcome<Option<Value>>;
}

impl IntoResult for () {
    fn into_result(self) -> Outcome<Option<Value>> {
        Ok(None)
    }
}

/// One `IntoResult` per reply type `commands.rs` names.
macro_rules! typed_replies {
    ($($reply:ty),+ $(,)?) => {$(
        impl IntoResult for $reply {
            fn into_result(self) -> Outcome<Option<Value>> {
                serde_json::to_value(self).map(Some).map_err(|error| {
                    BridgeError::failed(format!("The reply could not be encoded: {error}"))
                })
            }
        }
    )+};
}

typed_replies!(ConfirmReply, ChosenPathReply, SpeakerOptionsReply);

/// Writes [`BridgeHost`], the delegating `impl BridgeHost for Arc<T>` and
/// [`Dispatcher::call`] from one list of `Variant => fn name(params) ->
/// Reply;` lines, so a method is spelt once and the route over
/// [`BridgeMethod`] stays exhaustive: a variant without a line here does
/// not compile.
macro_rules! bridge_host {
    ($(
        $variant:ident => fn $method:ident($($param:ident: $params:ty)?) -> $reply:ty;
    )+) => {
        /// What answers the page's commands: one method per [`BridgeMethod`], params
        /// and reply typed per `commands.rs`. Every method defaults to the error the
        /// Swift window hosts throw for a method they do not route (`unknownMethod`,
        /// "The host does not answer `<method>`."), so a host implements what its
        /// window answers and nothing else. Swift: `BridgeHost`.
        pub trait BridgeHost: Send + Sync {
            $(
                #[allow(unused_variables)]
                fn $method(&self $(, $param: $params)?) -> Outcome<$reply> {
                    Err(BridgeError::not_answered(BridgeMethod::$variant))
                }
            )+
        }

        impl<T: BridgeHost + ?Sized> BridgeHost for std::sync::Arc<T> {
            $(
                fn $method(&self $(, $param: $params)?) -> Outcome<$reply> {
                    (**self).$method($($param)?)
                }
            )+
        }

        impl<H: BridgeHost> Dispatcher<H> {
            /// One method call: the params read as the method's contract type
            /// (a missing, `null` or unreadable value is `invalidParams`), the
            /// host's typed handler, and its reply as the envelope's `result`,
            /// `None` for a method without one. The Tauri `bridge_call` command
            /// maps this straight onto its own `Result`.
            pub fn call(&self, method: BridgeMethod, params: Option<Value>) -> Outcome<Option<Value>> {
                match method {
                    $(
                        BridgeMethod::$variant => {
                            $(let $param: $params = decode(method, params)?;)?
                            self.host.$method($($param)?).and_then(IntoResult::into_result)
                        }
                    )+
                }
            }
        }
    };
}

bridge_host! {
    PageReady => fn page_ready() -> ();
    PageLayout => fn page_layout(params: PageLayoutParams) -> ();

    MeetingsSetFilter => fn meetings_set_filter(params: SetFilterParams) -> ();
    MeetingsSetTagFilter => fn meetings_set_tag_filter(params: SetTagFilterParams) -> ();
    MeetingsSetQuery => fn meetings_set_query(params: SetQueryParams) -> ();
    MeetingsSelect => fn meetings_select(params: MeetingIdParams) -> ();
    MeetingsDelete => fn meetings_delete(params: MeetingIdParams) -> ConfirmReply;

    MeetingSetTab => fn meeting_set_tab(params: SetTabParams) -> ();
    MeetingSetTags => fn meeting_set_tags(params: SetTagsParams) -> ();
    MeetingSetTemplate => fn meeting_set_template(params: SetTemplateParams) -> ();
    MeetingRerunSummary => fn meeting_rerun_summary() -> ();
    MeetingReexport => fn meeting_reexport() -> ();
    MeetingProcessAgain => fn meeting_process_again() -> ();
    MeetingSetKeepAudio => fn meeting_set_keep_audio(params: SetBoolParams) -> ConfirmReply;
    MeetingDeleteRecordingNow => fn meeting_delete_recording_now() -> ConfirmReply;
    MeetingSaveNotes => fn meeting_save_notes(params: SaveNotesParams) -> ();
    MeetingRevealRecording => fn meeting_reveal_recording() -> ();
    MeetingRevealExport => fn meeting_reveal_export() -> ();

    SpeakersOptions => fn speakers_options(params: SpeakerOptionsParams) -> SpeakerOptionsReply;
    SpeakersSelect => fn speakers_select(params: SelectSpeakerParams) -> ();
    SpeakersPlay => fn speakers_play(params: SpeakerIdParams) -> ();
    SpeakersStop => fn speakers_stop() -> ();

    RecordingStart => fn recording_start(params: StartRecordingParams) -> ();
    RecordingStop => fn recording_stop() -> ();
    RecordingToggle => fn recording_toggle() -> ();
    RecordingKeepGoing => fn recording_keep_going() -> ();
    RecordingClearMessages => fn recording_clear_messages() -> ();

    SetupDismissBanner => fn setup_dismiss_banner() -> ();

    SettingsGeneralSetLaunchAtLogin => fn settings_general_set_launch_at_login(params: SetBoolParams) -> ();
    SettingsGeneralSetDetection => fn settings_general_set_detection(params: SetBoolParams) -> ();
    SettingsGeneralSetDefaultTemplate => fn settings_general_set_default_template(params: SetTemplateParams) -> ();
    SettingsGeneralRequestCalendar => fn settings_general_request_calendar() -> ();
    SettingsGeneralSetAutomaticUpdates => fn settings_general_set_automatic_updates(params: SetAutomaticUpdatesParams) -> ();
    SettingsGeneralOpenLoginItems => fn settings_general_open_login_items() -> ();
    SettingsRecordingSetInputDevice => fn settings_recording_set_input_device(params: SetStringParams) -> ();
    SettingsRecordingRefreshDevices => fn settings_recording_refresh_devices() -> ();
    SettingsRecordingChooseFolder => fn settings_recording_choose_folder() -> ChosenPathReply;
    SettingsRecordingRevealFolder => fn settings_recording_reveal_folder() -> ();
    SettingsRecordingSetRetention => fn settings_recording_set_retention(params: SetRetentionParams) -> ();
    SettingsRecordingRequestPermission => fn settings_recording_request_permission(params: PermissionKindParams) -> ();
    SettingsTranscriptionSetEngine => fn settings_transcription_set_engine(params: SetStringParams) -> ();
    SettingsTranscriptionDownload => fn settings_transcription_download(params: AssetIdParams) -> ();
    SettingsTranscriptionRemove => fn settings_transcription_remove(params: AssetIdParams) -> ();
    SettingsSummariesSelectPreset => fn settings_summaries_select_preset(params: SetStringParams) -> ();
    SettingsSummariesUpdate => fn settings_summaries_update(params: SummariesUpdateParams) -> ();
    SettingsSummariesSave => fn settings_summaries_save() -> ();
    SettingsSummariesTest => fn settings_summaries_test() -> ();
    SettingsSummariesConfirmCodex => fn settings_summaries_confirm_codex() -> ();
    SettingsSummariesRefreshCodexStatus => fn settings_summaries_refresh_codex_status() -> ();
    SettingsSummariesRefreshCodexModels => fn settings_summaries_refresh_codex_models() -> ();
    SettingsSummariesSelectCodexModel => fn settings_summaries_select_codex_model(params: SetStringParams) -> ();
    SettingsSummariesStopUsingCodex => fn settings_summaries_stop_using_codex() -> ();
    SettingsExportSetEnabled => fn settings_export_set_enabled(params: SetBoolParams) -> ();
    SettingsExportChooseVault => fn settings_export_choose_vault() -> ChosenPathReply;
    SettingsExportUpdate => fn settings_export_update(params: ExportUpdateParams) -> ();
    SettingsExportSave => fn settings_export_save() -> ();
    SettingsPhoneBeginPairing => fn settings_phone_begin_pairing() -> ();
    SettingsPhoneCancelPairing => fn settings_phone_cancel_pairing() -> ();
    SettingsPhoneRevoke => fn settings_phone_revoke(params: DeviceIdParams) -> ();

    OnboardingRequest => fn onboarding_request(params: PermissionKindParams) -> ();
    OnboardingSkip => fn onboarding_skip(params: PermissionKindParams) -> ();
    OnboardingRefresh => fn onboarding_refresh() -> ();
    OnboardingAdvance => fn onboarding_advance() -> ();
    OnboardingBack => fn onboarding_back() -> ();
    OnboardingSaveSummaries => fn onboarding_save_summaries() -> ();
    OnboardingConfirmSummariesWithCodex => fn onboarding_confirm_summaries_with_codex() -> ();
    OnboardingChooseVault => fn onboarding_choose_vault() -> ChosenPathReply;
    OnboardingSaveVault => fn onboarding_save_vault() -> ();
    OnboardingSkipSetup => fn onboarding_skip_setup(params: SetupStepParams) -> ();
    OnboardingFinish => fn onboarding_finish() -> ();

    UpdatesCheck => fn updates_check() -> ();
    SystemOpenUrl => fn system_open_url(params: OpenUrlParams) -> ();
    SystemOpenSystemSettings => fn system_open_system_settings(params: PermissionKindParams) -> ();
    WindowOpen => fn window_open(params: WindowParams) -> ();
    WindowClose => fn window_close(params: WindowParams) -> ();
    UiConfirmDestructive => fn ui_confirm_destructive(params: ConfirmDestructiveParams) -> ConfirmReply;
}

/// Routes a method to the host's typed handler and wraps the outcome; see
/// [`Dispatcher::call`] and [`Dispatcher::dispatch`]. Swift: `BridgeDispatcher`.
pub struct Dispatcher<H: BridgeHost> {
    host: H,
}

impl<H: BridgeHost> Dispatcher<H> {
    pub fn new(host: H) -> Self {
        Self { host }
    }

    /// One envelope end to end: read the request, [`call`](Self::call),
    /// wrap. Never fails, so the page's promise resolves with an envelope in
    /// every case: a method name outside the contract is `unknownMethod`,
    /// a body that is not a request is `invalidParams`, with the body's `id`
    /// when it was readable.
    pub fn dispatch(&self, body: &str) -> BridgeReply {
        let request = match decode_request(body) {
            Ok(request) => request,
            Err(reply) => return reply,
        };
        match self.call(request.method, request.params) {
            Ok(Some(result)) => BridgeReply::result(request.id, result),
            Ok(None) => BridgeReply::empty(request.id),
            Err(error) => BridgeReply::error(request.id, error),
        }
    }

    /// `dispatch` with the reply encoded in the dispatcher's compact style.
    ///
    /// ```
    /// use steno_bridge::{BridgeHost, Dispatcher, Outcome};
    ///
    /// struct Host;
    ///
    /// impl BridgeHost for Host {
    ///     fn recording_stop(&self) -> Outcome<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let dispatcher = Dispatcher::new(Host);
    /// assert_eq!(
    ///     dispatcher.dispatch_json(r#"{"id":"req-1","method":"recording.stop","params":null}"#),
    ///     r#"{"id":"req-1"}"#
    /// );
    /// assert_eq!(
    ///     dispatcher.dispatch_json(r#"{"id":"req-2","method":"recording.start","params":{"mode":"call"}}"#),
    ///     r#"{"error":{"code":"unknownMethod","message":"The host does not answer recording.start."},"id":"req-2"}"#
    /// );
    /// ```
    pub fn dispatch_json(&self, body: &str) -> String {
        json::to_compact_string(&self.dispatch(body))
            .expect("an envelope of strings and JSON values encodes")
    }
}

/// The envelope before the method name is checked, so an unknown method and
/// a malformed request are told apart by one read. Swift: `RawRequest`.
#[derive(serde::Deserialize)]
struct RawRequest {
    id: String,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

/// Reads a request body, or the reply to send instead. Swift:
/// `BridgeDispatcher.decode`.
fn decode_request(body: &str) -> Result<BridgeRequest, BridgeReply> {
    let invalid =
        |id: &str, message: &str| Err(BridgeReply::error(id, BridgeError::invalid_params(message)));
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return invalid("", "The message is not JSON.");
    };
    if !value.is_object() {
        return invalid("", "The message is not an object.");
    }
    let id = value["id"].as_str().unwrap_or("").to_owned();
    let Ok(raw) = serde_json::from_value::<RawRequest>(value) else {
        return invalid(&id, "The message is not a bridge request.");
    };
    match raw.method.parse::<BridgeMethod>() {
        Ok(method) => Ok(BridgeRequest::new(raw.id, method, raw.params)),
        Err(_) => Err(BridgeReply::error(
            raw.id,
            BridgeError::unknown_method(format!("Unknown method '{}'.", raw.method)),
        )),
    }
}

/// The method's params as its contract type; a missing, `null` or unreadable
/// value is `invalidParams`. Swift: `BridgeRequest.params(_:)`. Public so a
/// shell that answers `window.*` and `system.openURL` itself, before the
/// dispatcher, reads their params with the same errors (`"<method> needs
/// params."`) the dispatcher would give.
pub fn decode<T: DeserializeOwned>(method: BridgeMethod, params: Option<Value>) -> Outcome<T> {
    match params {
        None | Some(Value::Null) => Err(BridgeError::invalid_params(format!(
            "{method} needs params."
        ))),
        Some(value) => serde_json::from_value(value)
            .map_err(|error| BridgeError::invalid_params(format!("{method}: {error}"))),
    }
}
