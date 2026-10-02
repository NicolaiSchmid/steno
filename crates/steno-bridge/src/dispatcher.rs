//! The host side of a call, after `apps/macos/Steno/Web/BridgeDispatcher.swift`
//! and `BridgeRequestParams.swift`: a request body in, a reply envelope out.
//! [`BridgeHost`] has one typed method per [`BridgeMethod`]; the
//! [`Dispatcher`] decodes the envelope, reads the params as the method's
//! contract type, calls the host and wraps the outcome, so the page's promise
//! always settles with an envelope, never a rejection.
//!
//! Calls are synchronous and take `&self`: the shell runs the dispatcher off
//! its UI thread and the host uses interior mutability for its view models,
//! which is what the Swift `@MainActor` host amounts to.

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
use crate::envelope::{
    BridgeError, BridgeEvent, BridgeMethod, BridgeReply, BridgeRequest, BridgeTopic,
};
use crate::json;
use crate::snapshots::Snapshot;

/// Where a host publishes snapshots. Swift: `BridgeEventSink`.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: BridgeEvent);
}

/// The typed publish path over any [`EventSink`].
pub trait EventSinkExt: EventSink {
    /// Publishes a snapshot on its own topic.
    fn publish<S: Snapshot + ?Sized>(&self, snapshot: &S) -> Result<(), serde_json::Error> {
        self.emit(BridgeEvent::snapshot(S::TOPIC, snapshot)?);
        Ok(())
    }

    /// Publishes any payload on a topic; for `null` and for tests.
    fn publish_value<T: Serialize + ?Sized>(
        &self,
        topic: BridgeTopic,
        payload: &T,
    ) -> Result<(), serde_json::Error> {
        self.emit(BridgeEvent::snapshot(topic, payload)?);
        Ok(())
    }
}

impl<T: EventSink + ?Sized> EventSinkExt for T {}

/// A method's outcome as the dispatcher wraps it: a value, nothing, or a
/// contract error.
pub type Outcome<T> = Result<T, BridgeError>;

macro_rules! unanswered {
    ($method:ident) => {
        Err(BridgeError::not_answered(BridgeMethod::$method))
    };
}

/// What answers the page's commands: one method per [`BridgeMethod`], params
/// and reply typed per `commands.rs`. Every method defaults to the error the
/// Swift window hosts throw for a method they do not route (`unknownMethod`,
/// "The host does not answer <method>."), so a host implements what its
/// window answers and nothing else. Swift: `BridgeHost`.
#[allow(unused_variables)]
pub trait BridgeHost: Send + Sync {
    fn page_ready(&self) -> Outcome<()> {
        unanswered!(PageReady)
    }
    fn page_layout(&self, params: PageLayoutParams) -> Outcome<()> {
        unanswered!(PageLayout)
    }

    fn meetings_set_filter(&self, params: SetFilterParams) -> Outcome<()> {
        unanswered!(MeetingsSetFilter)
    }
    fn meetings_set_tag_filter(&self, params: SetTagFilterParams) -> Outcome<()> {
        unanswered!(MeetingsSetTagFilter)
    }
    fn meetings_set_query(&self, params: SetQueryParams) -> Outcome<()> {
        unanswered!(MeetingsSetQuery)
    }
    fn meetings_select(&self, params: MeetingIdParams) -> Outcome<()> {
        unanswered!(MeetingsSelect)
    }
    fn meetings_delete(&self, params: MeetingIdParams) -> Outcome<ConfirmReply> {
        unanswered!(MeetingsDelete)
    }

    fn meeting_set_tab(&self, params: SetTabParams) -> Outcome<()> {
        unanswered!(MeetingSetTab)
    }
    fn meeting_set_tags(&self, params: SetTagsParams) -> Outcome<()> {
        unanswered!(MeetingSetTags)
    }
    fn meeting_set_template(&self, params: SetTemplateParams) -> Outcome<()> {
        unanswered!(MeetingSetTemplate)
    }
    fn meeting_rerun_summary(&self) -> Outcome<()> {
        unanswered!(MeetingRerunSummary)
    }
    fn meeting_reexport(&self) -> Outcome<()> {
        unanswered!(MeetingReexport)
    }
    fn meeting_set_keep_audio(&self, params: SetBoolParams) -> Outcome<ConfirmReply> {
        unanswered!(MeetingSetKeepAudio)
    }
    fn meeting_delete_recording_now(&self) -> Outcome<ConfirmReply> {
        unanswered!(MeetingDeleteRecordingNow)
    }
    fn meeting_save_notes(&self, params: SaveNotesParams) -> Outcome<()> {
        unanswered!(MeetingSaveNotes)
    }
    fn meeting_reveal_recording(&self) -> Outcome<()> {
        unanswered!(MeetingRevealRecording)
    }
    fn meeting_reveal_export(&self) -> Outcome<()> {
        unanswered!(MeetingRevealExport)
    }

    fn speakers_options(&self, params: SpeakerOptionsParams) -> Outcome<SpeakerOptionsReply> {
        unanswered!(SpeakersOptions)
    }
    fn speakers_select(&self, params: SelectSpeakerParams) -> Outcome<()> {
        unanswered!(SpeakersSelect)
    }
    fn speakers_play(&self, params: SpeakerIdParams) -> Outcome<()> {
        unanswered!(SpeakersPlay)
    }
    fn speakers_stop(&self) -> Outcome<()> {
        unanswered!(SpeakersStop)
    }

    fn recording_start(&self, params: StartRecordingParams) -> Outcome<()> {
        unanswered!(RecordingStart)
    }
    fn recording_stop(&self) -> Outcome<()> {
        unanswered!(RecordingStop)
    }
    fn recording_toggle(&self) -> Outcome<()> {
        unanswered!(RecordingToggle)
    }
    fn recording_keep_going(&self) -> Outcome<()> {
        unanswered!(RecordingKeepGoing)
    }
    fn recording_clear_messages(&self) -> Outcome<()> {
        unanswered!(RecordingClearMessages)
    }

    fn setup_dismiss_banner(&self) -> Outcome<()> {
        unanswered!(SetupDismissBanner)
    }

    fn settings_general_set_launch_at_login(&self, params: SetBoolParams) -> Outcome<()> {
        unanswered!(SettingsGeneralSetLaunchAtLogin)
    }
    fn settings_general_set_detection(&self, params: SetBoolParams) -> Outcome<()> {
        unanswered!(SettingsGeneralSetDetection)
    }
    fn settings_general_set_default_template(&self, params: SetTemplateParams) -> Outcome<()> {
        unanswered!(SettingsGeneralSetDefaultTemplate)
    }
    fn settings_general_request_calendar(&self) -> Outcome<()> {
        unanswered!(SettingsGeneralRequestCalendar)
    }
    fn settings_general_set_automatic_updates(
        &self,
        params: SetAutomaticUpdatesParams,
    ) -> Outcome<()> {
        unanswered!(SettingsGeneralSetAutomaticUpdates)
    }
    fn settings_general_open_login_items(&self) -> Outcome<()> {
        unanswered!(SettingsGeneralOpenLoginItems)
    }
    fn settings_recording_set_input_device(&self, params: SetStringParams) -> Outcome<()> {
        unanswered!(SettingsRecordingSetInputDevice)
    }
    fn settings_recording_refresh_devices(&self) -> Outcome<()> {
        unanswered!(SettingsRecordingRefreshDevices)
    }
    fn settings_recording_choose_folder(&self) -> Outcome<ChosenPathReply> {
        unanswered!(SettingsRecordingChooseFolder)
    }
    fn settings_recording_reveal_folder(&self) -> Outcome<()> {
        unanswered!(SettingsRecordingRevealFolder)
    }
    fn settings_recording_set_retention(&self, params: SetRetentionParams) -> Outcome<()> {
        unanswered!(SettingsRecordingSetRetention)
    }
    fn settings_recording_request_permission(&self, params: PermissionKindParams) -> Outcome<()> {
        unanswered!(SettingsRecordingRequestPermission)
    }
    fn settings_transcription_set_engine(&self, params: SetStringParams) -> Outcome<()> {
        unanswered!(SettingsTranscriptionSetEngine)
    }
    fn settings_transcription_download(&self, params: AssetIdParams) -> Outcome<()> {
        unanswered!(SettingsTranscriptionDownload)
    }
    fn settings_transcription_remove(&self, params: AssetIdParams) -> Outcome<()> {
        unanswered!(SettingsTranscriptionRemove)
    }
    fn settings_summaries_select_preset(&self, params: SetStringParams) -> Outcome<()> {
        unanswered!(SettingsSummariesSelectPreset)
    }
    fn settings_summaries_update(&self, params: SummariesUpdateParams) -> Outcome<()> {
        unanswered!(SettingsSummariesUpdate)
    }
    fn settings_summaries_save(&self) -> Outcome<()> {
        unanswered!(SettingsSummariesSave)
    }
    fn settings_summaries_test(&self) -> Outcome<()> {
        unanswered!(SettingsSummariesTest)
    }
    fn settings_summaries_confirm_codex(&self) -> Outcome<()> {
        unanswered!(SettingsSummariesConfirmCodex)
    }
    fn settings_summaries_refresh_codex_status(&self) -> Outcome<()> {
        unanswered!(SettingsSummariesRefreshCodexStatus)
    }
    fn settings_summaries_refresh_codex_models(&self) -> Outcome<()> {
        unanswered!(SettingsSummariesRefreshCodexModels)
    }
    fn settings_summaries_select_codex_model(&self, params: SetStringParams) -> Outcome<()> {
        unanswered!(SettingsSummariesSelectCodexModel)
    }
    fn settings_summaries_stop_using_codex(&self) -> Outcome<()> {
        unanswered!(SettingsSummariesStopUsingCodex)
    }
    fn settings_export_set_enabled(&self, params: SetBoolParams) -> Outcome<()> {
        unanswered!(SettingsExportSetEnabled)
    }
    fn settings_export_choose_vault(&self) -> Outcome<ChosenPathReply> {
        unanswered!(SettingsExportChooseVault)
    }
    fn settings_export_update(&self, params: ExportUpdateParams) -> Outcome<()> {
        unanswered!(SettingsExportUpdate)
    }
    fn settings_export_save(&self) -> Outcome<()> {
        unanswered!(SettingsExportSave)
    }
    fn settings_phone_begin_pairing(&self) -> Outcome<()> {
        unanswered!(SettingsPhoneBeginPairing)
    }
    fn settings_phone_cancel_pairing(&self) -> Outcome<()> {
        unanswered!(SettingsPhoneCancelPairing)
    }
    fn settings_phone_revoke(&self, params: DeviceIdParams) -> Outcome<()> {
        unanswered!(SettingsPhoneRevoke)
    }

    fn onboarding_request(&self, params: PermissionKindParams) -> Outcome<()> {
        unanswered!(OnboardingRequest)
    }
    fn onboarding_skip(&self, params: PermissionKindParams) -> Outcome<()> {
        unanswered!(OnboardingSkip)
    }
    fn onboarding_refresh(&self) -> Outcome<()> {
        unanswered!(OnboardingRefresh)
    }
    fn onboarding_advance(&self) -> Outcome<()> {
        unanswered!(OnboardingAdvance)
    }
    fn onboarding_back(&self) -> Outcome<()> {
        unanswered!(OnboardingBack)
    }
    fn onboarding_save_summaries(&self) -> Outcome<()> {
        unanswered!(OnboardingSaveSummaries)
    }
    fn onboarding_confirm_summaries_with_codex(&self) -> Outcome<()> {
        unanswered!(OnboardingConfirmSummariesWithCodex)
    }
    fn onboarding_choose_vault(&self) -> Outcome<ChosenPathReply> {
        unanswered!(OnboardingChooseVault)
    }
    fn onboarding_save_vault(&self) -> Outcome<()> {
        unanswered!(OnboardingSaveVault)
    }
    fn onboarding_skip_setup(&self, params: SetupStepParams) -> Outcome<()> {
        unanswered!(OnboardingSkipSetup)
    }
    fn onboarding_finish(&self) -> Outcome<()> {
        unanswered!(OnboardingFinish)
    }

    fn updates_check(&self) -> Outcome<()> {
        unanswered!(UpdatesCheck)
    }
    fn system_open_url(&self, params: OpenUrlParams) -> Outcome<()> {
        unanswered!(SystemOpenUrl)
    }
    fn system_open_system_settings(&self, params: PermissionKindParams) -> Outcome<()> {
        unanswered!(SystemOpenSystemSettings)
    }
    fn window_open(&self, params: WindowParams) -> Outcome<()> {
        unanswered!(WindowOpen)
    }
    fn window_close(&self, params: WindowParams) -> Outcome<()> {
        unanswered!(WindowClose)
    }
    fn ui_confirm_destructive(&self, params: ConfirmDestructiveParams) -> Outcome<ConfirmReply> {
        unanswered!(UiConfirmDestructive)
    }
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
pub struct Dispatcher<H: BridgeHost + ?Sized> {
    host: H,
}

impl<H: BridgeHost> Dispatcher<H> {
    pub fn new(host: H) -> Self {
        Self { host }
    }

    pub fn into_host(self) -> H {
        self.host
    }
}

impl<H: BridgeHost + ?Sized> Dispatcher<H> {
    pub fn host(&self) -> &H {
        &self.host
    }

    /// Reads a request body. A method name outside the contract is
    /// `unknownMethod`; anything else that fails to decode is
    /// `invalidParams`, with the body's `id` when it was readable.
    pub fn decode(body: &str) -> Decoding {
        let value: Value = match serde_json::from_str(body) {
            Ok(value) => value,
            Err(_) => {
                return Decoding::Rejected(BridgeReply::error(
                    "",
                    BridgeError::invalid_params("The message is not JSON."),
                ));
            }
        };
        let Some(object) = value.as_object() else {
            return Decoding::Rejected(BridgeReply::error(
                "",
                BridgeError::invalid_params("The message is not an object."),
            ));
        };
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let Ok(raw) = serde_json::from_value::<RawRequest>(value.clone()) else {
            return Decoding::Rejected(BridgeReply::error(
                id,
                BridgeError::invalid_params("The message is not a bridge request."),
            ));
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
        match Self::decode(body) {
            Decoding::Request(request) => self.dispatch_request(&request),
            Decoding::Rejected(reply) => reply,
        }
    }

    /// `dispatch` with the reply encoded in the dispatcher's compact style.
    pub fn dispatch_json(&self, body: &str) -> String {
        let reply = self.dispatch(body);
        json::to_compact_string(&reply).unwrap_or_else(|_| {
            json::to_compact_string(&BridgeReply::error(
                reply.id,
                BridgeError::failed("The reply could not be encoded."),
            ))
            .expect("an error envelope of strings encodes")
        })
    }

    /// Routes a decoded request and wraps the outcome.
    pub fn dispatch_request(&self, request: &BridgeRequest) -> BridgeReply {
        BridgeReply::from_outcome(request.id.clone(), self.handle(request))
    }

    /// Routes by method: reads the params as the contract type, calls the
    /// host, converts a typed reply to its JSON value.
    #[allow(clippy::too_many_lines)]
    pub fn handle(&self, request: &BridgeRequest) -> Outcome<Option<Value>> {
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
pub fn params<T: DeserializeOwned>(request: &BridgeRequest) -> Outcome<T> {
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
