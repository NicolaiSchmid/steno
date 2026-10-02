//! The host side of a call, after `apps/macos/Steno/Web/BridgeDispatcher.swift`
//! and `BridgeRequestParams.swift`: a request body in, a reply envelope out.
//! [`BridgeHost`] has one typed method per [`BridgeMethod`]; the
//! [`Dispatcher`] decodes the envelope, reads the params as the method's
//! contract type, calls the host and wraps the outcome, so the page's promise
//! always settles with an envelope, never a rejection.
//!
//! Hosts run blocking. Every method takes `&self` and returns when the work
//! is done, including a method that waits on a dialog; the host uses interior
//! mutability for its view models, which is what the Swift `@MainActor` host
//! amounts to. The Tauri shell runs [`Dispatcher::dispatch`] on a blocking
//! thread (the command handler is `async` and awaits a `spawn_blocking`) and
//! bounces anything that needs the UI, such as `ui.confirmDestructive` and
//! the folder panels, to the main thread from inside the host method. A host
//! behind an `Arc` is a host ([`BridgeHost`] is implemented for
//! `Arc<T: BridgeHost + ?Sized>`), so the shell can share one host between
//! the dispatcher and the event publisher.

use serde::Serialize;
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

/// Writes [`BridgeHost`] from one list of `Variant => fn name(params) -> Reply;`
/// lines, and the delegating `impl BridgeHost for Arc<T>` from the same list,
/// so a method is spelt once.
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

/// The envelope before the method name is checked, so an unknown method and
/// a malformed request are told apart by one decode. Swift: `RawRequest`.
#[derive(serde::Deserialize)]
struct RawRequest {
    id: String,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

/// The result of reading a request body. Swift: `BridgeDispatcher.Decoding`.
#[derive(Debug, Clone, PartialEq)]
pub enum Decoding {
    Request(BridgeRequest),
    /// The reply to send instead; `id` is the body's when it was readable.
    Rejected(BridgeReply),
}

/// Reads the envelope, routes by method to the host's typed handler, and
/// wraps the outcome. Swift: `BridgeDispatcher`.
pub struct Dispatcher<H: BridgeHost> {
    host: H,
}

impl<H: BridgeHost> Dispatcher<H> {
    pub fn new(host: H) -> Self {
        Self { host }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    /// Reads a request body. A method name outside the contract is
    /// `unknownMethod`; anything else that fails to decode is
    /// `invalidParams`, with the body's `id` when it was readable.
    pub fn decode(body: &str) -> Decoding {
        let invalid = |id: &str, message: &str| {
            Decoding::Rejected(BridgeReply::error(id, BridgeError::invalid_params(message)))
        };
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
            Ok(method) => Decoding::Request(BridgeRequest::new(raw.id, method, raw.params)),
            Err(_) => Decoding::Rejected(BridgeReply::error(
                raw.id,
                BridgeError::unknown_method(format!("Unknown method '{}'.", raw.method)),
            )),
        }
    }

    /// One call end to end: decode, route, wrap. Never fails, so the page's
    /// promise resolves with an envelope in every case.
    pub fn dispatch(&self, body: &str) -> BridgeReply {
        let request = match Self::decode(body) {
            Decoding::Request(request) => request,
            Decoding::Rejected(reply) => return reply,
        };
        match self.route(&request) {
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

    /// Routes by method: reads the params as the contract type, calls the
    /// host, converts a typed reply to its JSON value.
    #[allow(clippy::too_many_lines)]
    fn route(&self, request: &BridgeRequest) -> Outcome<Option<Value>> {
        use BridgeMethod as M;
        let host = &self.host;
        match request.method {
            M::PageReady => unit(host.page_ready()),
            M::PageLayout => unit(host.page_layout(params(request)?)),

            M::MeetingsSetFilter => unit(host.meetings_set_filter(params(request)?)),
            M::MeetingsSetTagFilter => unit(host.meetings_set_tag_filter(params(request)?)),
            M::MeetingsSetQuery => unit(host.meetings_set_query(params(request)?)),
            M::MeetingsSelect => unit(host.meetings_select(params(request)?)),
            M::MeetingsDelete => value(host.meetings_delete(params(request)?)),

            M::MeetingSetTab => unit(host.meeting_set_tab(params(request)?)),
            M::MeetingSetTags => unit(host.meeting_set_tags(params(request)?)),
            M::MeetingSetTemplate => unit(host.meeting_set_template(params(request)?)),
            M::MeetingRerunSummary => unit(host.meeting_rerun_summary()),
            M::MeetingReexport => unit(host.meeting_reexport()),
            M::MeetingSetKeepAudio => value(host.meeting_set_keep_audio(params(request)?)),
            M::MeetingDeleteRecordingNow => value(host.meeting_delete_recording_now()),
            M::MeetingSaveNotes => unit(host.meeting_save_notes(params(request)?)),
            M::MeetingRevealRecording => unit(host.meeting_reveal_recording()),
            M::MeetingRevealExport => unit(host.meeting_reveal_export()),

            M::SpeakersOptions => value(host.speakers_options(params(request)?)),
            M::SpeakersSelect => unit(host.speakers_select(params(request)?)),
            M::SpeakersPlay => unit(host.speakers_play(params(request)?)),
            M::SpeakersStop => unit(host.speakers_stop()),

            M::RecordingStart => unit(host.recording_start(params(request)?)),
            M::RecordingStop => unit(host.recording_stop()),
            M::RecordingToggle => unit(host.recording_toggle()),
            M::RecordingKeepGoing => unit(host.recording_keep_going()),
            M::RecordingClearMessages => unit(host.recording_clear_messages()),

            M::SetupDismissBanner => unit(host.setup_dismiss_banner()),

            M::SettingsGeneralSetLaunchAtLogin => {
                unit(host.settings_general_set_launch_at_login(params(request)?))
            }
            M::SettingsGeneralSetDetection => {
                unit(host.settings_general_set_detection(params(request)?))
            }
            M::SettingsGeneralSetDefaultTemplate => {
                unit(host.settings_general_set_default_template(params(request)?))
            }
            M::SettingsGeneralRequestCalendar => unit(host.settings_general_request_calendar()),
            M::SettingsGeneralSetAutomaticUpdates => {
                unit(host.settings_general_set_automatic_updates(params(request)?))
            }
            M::SettingsGeneralOpenLoginItems => unit(host.settings_general_open_login_items()),
            M::SettingsRecordingSetInputDevice => {
                unit(host.settings_recording_set_input_device(params(request)?))
            }
            M::SettingsRecordingRefreshDevices => unit(host.settings_recording_refresh_devices()),
            M::SettingsRecordingChooseFolder => value(host.settings_recording_choose_folder()),
            M::SettingsRecordingRevealFolder => unit(host.settings_recording_reveal_folder()),
            M::SettingsRecordingSetRetention => {
                unit(host.settings_recording_set_retention(params(request)?))
            }
            M::SettingsRecordingRequestPermission => {
                unit(host.settings_recording_request_permission(params(request)?))
            }
            M::SettingsTranscriptionSetEngine => {
                unit(host.settings_transcription_set_engine(params(request)?))
            }
            M::SettingsTranscriptionDownload => {
                unit(host.settings_transcription_download(params(request)?))
            }
            M::SettingsTranscriptionRemove => {
                unit(host.settings_transcription_remove(params(request)?))
            }
            M::SettingsSummariesSelectPreset => {
                unit(host.settings_summaries_select_preset(params(request)?))
            }
            M::SettingsSummariesUpdate => unit(host.settings_summaries_update(params(request)?)),
            M::SettingsSummariesSave => unit(host.settings_summaries_save()),
            M::SettingsSummariesTest => unit(host.settings_summaries_test()),
            M::SettingsSummariesConfirmCodex => unit(host.settings_summaries_confirm_codex()),
            M::SettingsSummariesRefreshCodexStatus => {
                unit(host.settings_summaries_refresh_codex_status())
            }
            M::SettingsSummariesRefreshCodexModels => {
                unit(host.settings_summaries_refresh_codex_models())
            }
            M::SettingsSummariesSelectCodexModel => {
                unit(host.settings_summaries_select_codex_model(params(request)?))
            }
            M::SettingsSummariesStopUsingCodex => unit(host.settings_summaries_stop_using_codex()),
            M::SettingsExportSetEnabled => unit(host.settings_export_set_enabled(params(request)?)),
            M::SettingsExportChooseVault => value(host.settings_export_choose_vault()),
            M::SettingsExportUpdate => unit(host.settings_export_update(params(request)?)),
            M::SettingsExportSave => unit(host.settings_export_save()),
            M::SettingsPhoneBeginPairing => unit(host.settings_phone_begin_pairing()),
            M::SettingsPhoneCancelPairing => unit(host.settings_phone_cancel_pairing()),
            M::SettingsPhoneRevoke => unit(host.settings_phone_revoke(params(request)?)),

            M::OnboardingRequest => unit(host.onboarding_request(params(request)?)),
            M::OnboardingSkip => unit(host.onboarding_skip(params(request)?)),
            M::OnboardingRefresh => unit(host.onboarding_refresh()),
            M::OnboardingAdvance => unit(host.onboarding_advance()),
            M::OnboardingBack => unit(host.onboarding_back()),
            M::OnboardingSaveSummaries => unit(host.onboarding_save_summaries()),
            M::OnboardingConfirmSummariesWithCodex => {
                unit(host.onboarding_confirm_summaries_with_codex())
            }
            M::OnboardingChooseVault => value(host.onboarding_choose_vault()),
            M::OnboardingSaveVault => unit(host.onboarding_save_vault()),
            M::OnboardingSkipSetup => unit(host.onboarding_skip_setup(params(request)?)),
            M::OnboardingFinish => unit(host.onboarding_finish()),

            M::UpdatesCheck => unit(host.updates_check()),
            M::SystemOpenUrl => unit(host.system_open_url(params(request)?)),
            M::SystemOpenSystemSettings => unit(host.system_open_system_settings(params(request)?)),
            M::WindowOpen => unit(host.window_open(params(request)?)),
            M::WindowClose => unit(host.window_close(params(request)?)),
            M::UiConfirmDestructive => value(host.ui_confirm_destructive(params(request)?)),
        }
    }
}

/// The method's params decoded into its contract type; a missing, `null` or
/// unreadable value is `invalidParams`. Swift: `BridgeRequest.params(_:)`.
fn params<T: DeserializeOwned>(request: &BridgeRequest) -> Outcome<T> {
    match &request.params {
        None | Some(Value::Null) => Err(BridgeError::invalid_params(format!(
            "{} needs params.",
            request.method
        ))),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| BridgeError::invalid_params(format!("{}: {error}", request.method))),
    }
}

fn unit(outcome: Outcome<()>) -> Outcome<Option<Value>> {
    outcome.map(|()| None)
}

/// A typed reply as the envelope carries it. Swift: `BridgeReplies.value`.
fn value<T: Serialize>(outcome: Outcome<T>) -> Outcome<Option<Value>> {
    let reply = outcome?;
    serde_json::to_value(reply)
        .map(Some)
        .map_err(|error| BridgeError::failed(format!("The reply could not be encoded: {error}")))
}
