//! Topics, methods and the four envelopes, after
//! `Sources/StenoBridge/BridgeEnvelope.swift`. Snapshots flow host to page as
//! events; commands flow page to host as method calls with a reply.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::string_enum::string_enum;

string_enum! {
    /// Topics the host publishes; every publish carries a full snapshot.
    /// Swift: `BridgeTopic`.
    pub enum BridgeTopic {
        App = "app",
        Recording = "recording",
        Progress = "progress",
        MeetingsList = "meetings.list",
        MeetingDetail = "meeting.detail",
        SettingsGeneral = "settings.general",
        SettingsRecording = "settings.recording",
        SettingsTranscription = "settings.transcription",
        SettingsSummaries = "settings.summaries",
        SettingsExport = "settings.export",
        SettingsPhone = "settings.iphone",
        Onboarding = "onboarding",
    }
}

string_enum! {
    /// Methods the page may call; the params and reply types are named in
    /// `commands.rs` and routed in `dispatcher.rs`. Swift: `BridgeMethod`.
    pub enum BridgeMethod {
        PageReady = "page.ready",
        PageLayout = "page.layout",

        MeetingsSetFilter = "meetings.setFilter",
        MeetingsSetTagFilter = "meetings.setTagFilter",
        MeetingsSetQuery = "meetings.setQuery",
        MeetingsSelect = "meetings.select",
        MeetingsDelete = "meetings.delete",

        MeetingSetTab = "meeting.setTab",
        MeetingSetTags = "meeting.setTags",
        MeetingSetTemplate = "meeting.setTemplate",
        MeetingRerunSummary = "meeting.rerunSummary",
        MeetingReexport = "meeting.reexport",
        MeetingSetKeepAudio = "meeting.setKeepAudio",
        MeetingDeleteRecordingNow = "meeting.deleteRecordingNow",
        MeetingSaveNotes = "meeting.saveNotes",
        MeetingRevealRecording = "meeting.revealRecording",
        MeetingRevealExport = "meeting.revealExport",

        SpeakersOptions = "speakers.options",
        SpeakersSelect = "speakers.select",
        SpeakersPlay = "speakers.play",
        SpeakersStop = "speakers.stop",

        RecordingStart = "recording.start",
        RecordingStop = "recording.stop",
        RecordingToggle = "recording.toggle",
        RecordingKeepGoing = "recording.keepGoing",
        RecordingClearMessages = "recording.clearMessages",

        SetupDismissBanner = "setup.dismissBanner",

        SettingsGeneralSetLaunchAtLogin = "settings.general.setLaunchAtLogin",
        SettingsGeneralSetDetection = "settings.general.setDetectionEnabled",
        SettingsGeneralSetDefaultTemplate = "settings.general.setDefaultTemplate",
        SettingsGeneralRequestCalendar = "settings.general.requestCalendar",
        SettingsGeneralSetAutomaticUpdates = "settings.general.setAutomaticUpdates",
        SettingsGeneralOpenLoginItems = "settings.general.openLoginItems",
        /// `SetStringParams`; an empty value means the system default input.
        SettingsRecordingSetInputDevice = "settings.recording.setInputDevice",
        SettingsRecordingRefreshDevices = "settings.recording.refreshDevices",
        SettingsRecordingChooseFolder = "settings.recording.chooseFolder",
        SettingsRecordingRevealFolder = "settings.recording.revealFolder",
        SettingsRecordingSetRetention = "settings.recording.setRetention",
        SettingsRecordingRequestPermission = "settings.recording.requestPermission",
        SettingsTranscriptionSetEngine = "settings.transcription.setEngine",
        SettingsTranscriptionDownload = "settings.transcription.download",
        SettingsTranscriptionRemove = "settings.transcription.remove",
        SettingsSummariesSelectPreset = "settings.summaries.selectPreset",
        SettingsSummariesUpdate = "settings.summaries.update",
        SettingsSummariesSave = "settings.summaries.save",
        SettingsSummariesTest = "settings.summaries.test",
        SettingsSummariesConfirmCodex = "settings.summaries.confirmCodex",
        SettingsSummariesRefreshCodexStatus = "settings.summaries.refreshCodexStatus",
        SettingsSummariesRefreshCodexModels = "settings.summaries.refreshCodexModels",
        /// `SetStringParams`: the model slug.
        SettingsSummariesSelectCodexModel = "settings.summaries.selectCodexModel",
        SettingsSummariesStopUsingCodex = "settings.summaries.stopUsingCodex",
        SettingsExportSetEnabled = "settings.export.setEnabled",
        SettingsExportChooseVault = "settings.export.chooseVault",
        SettingsExportUpdate = "settings.export.update",
        SettingsExportSave = "settings.export.save",
        SettingsPhoneBeginPairing = "settings.iphone.beginPairing",
        SettingsPhoneCancelPairing = "settings.iphone.cancelPairing",
        SettingsPhoneRevoke = "settings.iphone.revoke",

        OnboardingRequest = "onboarding.request",
        OnboardingSkip = "onboarding.skip",
        /// Reads every permission's state again ("Check again" after System Settings).
        OnboardingRefresh = "onboarding.refresh",
        OnboardingAdvance = "onboarding.advance",
        OnboardingBack = "onboarding.back",
        /// The onboarding window's own Summaries save (the row collapses once saved).
        OnboardingSaveSummaries = "onboarding.saveSummaries",
        OnboardingConfirmSummariesWithCodex = "onboarding.confirmSummariesWithCodex",
        /// The folder panel; a chosen folder is saved as the vault at once.
        /// Replies `ChosenPathReply`.
        OnboardingChooseVault = "onboarding.chooseVault",
        /// Saves the vault row as it stands (a retry after a refused folder).
        OnboardingSaveVault = "onboarding.saveVault",
        OnboardingSkipSetup = "onboarding.skipSetup",
        OnboardingFinish = "onboarding.finish",

        UpdatesCheck = "updates.check",
        SystemOpenUrl = "system.openURL",
        SystemOpenSystemSettings = "system.openSystemSettings",
        WindowOpen = "window.open",
        WindowClose = "window.close",
        UiConfirmDestructive = "ui.confirmDestructive",
    }
}

/// One call from the page. `id` is echoed in the reply so the page can match
/// promises; `params` is the method's params type, or absent / `null`.
/// Swift: `BridgeRequest`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeRequest {
    pub id: String,
    pub method: BridgeMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl BridgeRequest {
    pub fn new(id: impl Into<String>, method: BridgeMethod, params: Option<Value>) -> Self {
        Self {
            id: id.into(),
            method,
            params,
        }
    }
}

/// The host's answer. At most one of `result` and `error` is set; neither for
/// a method without a reply value. Swift: `BridgeReply`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeReply {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<BridgeError>,
}

impl BridgeReply {
    /// A reply without a value (Swift's `BridgeReply(id:)`).
    pub fn empty(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            result: None,
            error: None,
        }
    }

    pub fn result(id: impl Into<String>, result: Value) -> Self {
        Self {
            id: id.into(),
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: impl Into<String>, error: BridgeError) -> Self {
        Self {
            id: id.into(),
            result: None,
            error: Some(error),
        }
    }
}

string_enum! {
    /// Swift: `BridgeError.Code`.
    pub enum BridgeErrorCode {
        UnknownMethod = "unknownMethod",
        InvalidParams = "invalidParams",
        NotFound = "notFound",
        Failed = "failed",
        Cancelled = "cancelled",
    }
}

/// A contract or host error the page receives in the reply envelope.
/// Swift: `BridgeError`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct BridgeError {
    pub code: BridgeErrorCode,
    pub message: String,
}

impl BridgeError {
    pub fn new(code: BridgeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn unknown_method(message: impl Into<String>) -> Self {
        Self::new(BridgeErrorCode::UnknownMethod, message)
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(BridgeErrorCode::InvalidParams, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(BridgeErrorCode::NotFound, message)
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(BridgeErrorCode::Failed, message)
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(BridgeErrorCode::Cancelled, message)
    }

    /// What the Swift window hosts throw for a method they do not route
    /// ("The main window does not answer meeting.setTab."); the
    /// `BridgeHost` default implementations return it.
    pub fn not_answered(method: BridgeMethod) -> Self {
        Self::unknown_method(format!("The host does not answer {method}."))
    }
}

/// One publish from the host. The page dispatches on `topic` and decodes
/// `payload` as that topic's snapshot type. Swift: `BridgeEvent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeEvent {
    pub topic: BridgeTopic,
    pub payload: Value,
}

impl BridgeEvent {
    pub fn new(topic: BridgeTopic, payload: Value) -> Self {
        Self { topic, payload }
    }

    /// A snapshot as an event (Swift's `BridgeEvent(topic:snapshot:)`).
    pub fn snapshot<T: Serialize + ?Sized>(
        topic: BridgeTopic,
        snapshot: &T,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            topic,
            payload: serde_json::to_value(snapshot)?,
        })
    }
}

// Shared vocabulary.

string_enum! {
    /// Swift: `BridgePermissionKind`.
    pub enum PermissionKind {
        Microphone = "microphone",
        SystemAudio = "systemAudio",
        Calendar = "calendar",
        LocalNetwork = "localNetwork",
    }
}

string_enum! {
    /// Swift: `BridgePermissionState`.
    pub enum PermissionState {
        Unknown = "unknown",
        Granted = "granted",
        Denied = "denied",
    }
}

string_enum! {
    /// Swift: `BridgeSettingsSection`.
    pub enum SettingsSection {
        General = "general",
        Recording = "recording",
        Transcription = "transcription",
        Summaries = "summaries",
        Export = "export",
        Phone = "iphone",
    }
}

string_enum! {
    /// Swift: `BridgeWindow`.
    pub enum BridgeWindow {
        Main = "main",
        Settings = "settings",
        Onboarding = "onboarding",
    }
}

string_enum! {
    /// Swift: `BridgeMeetingSource`.
    pub enum MeetingSource {
        Call = "call",
        InPerson = "inPerson",
        Phone = "phone",
    }
}

string_enum! {
    /// Swift: `BridgeMeetingState`, an alias of `StenoCore.MeetingState.Kind`.
    pub enum MeetingState {
        Recording = "recording",
        Queued = "queued",
        Processing = "processing",
        Ready = "ready",
        Failed = "failed",
    }
}

string_enum! {
    /// Swift: `BridgeCaptureMode`.
    pub enum CaptureMode {
        Call = "call",
        InPerson = "inPerson",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::json;

    #[test]
    fn methods_and_topics_use_dotted_lower_camel_names() {
        fn dotted_lower_camel(raw: &str) -> bool {
            raw.split('.').all(|part| {
                let mut chars = part.chars();
                chars.next().is_some_and(|c| c.is_ascii_lowercase())
                    && chars.all(|c| c.is_ascii_alphanumeric())
            })
        }
        for method in BridgeMethod::ALL {
            assert!(dotted_lower_camel(method.as_str()), "{method}");
        }
        for topic in BridgeTopic::ALL {
            assert!(dotted_lower_camel(topic.as_str()), "{topic}");
        }
    }

    #[test]
    fn raw_values_round_trip_through_from_str_and_serde() {
        for method in BridgeMethod::ALL {
            assert_eq!(method.as_str().parse::<BridgeMethod>().unwrap(), *method);
            let encoded = serde_json::to_value(method).unwrap();
            assert_eq!(encoded, Value::String(method.as_str().to_owned()));
            assert_eq!(
                serde_json::from_value::<BridgeMethod>(encoded).unwrap(),
                *method
            );
        }
        for topic in BridgeTopic::ALL {
            assert_eq!(topic.as_str().parse::<BridgeTopic>().unwrap(), *topic);
        }
        let error = "meetings.explode".parse::<BridgeMethod>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "'meetings.explode' is not a BridgeMethod"
        );
    }

    #[test]
    fn error_codes_are_the_contract_strings() {
        let codes: Vec<&str> = BridgeErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            codes,
            [
                "unknownMethod",
                "invalidParams",
                "notFound",
                "failed",
                "cancelled"
            ]
        );
        let error = BridgeError::not_answered(BridgeMethod::MeetingSetTab);
        assert_eq!(error.code, BridgeErrorCode::UnknownMethod);
        assert_eq!(error.message, "The host does not answer meeting.setTab.");
        assert_eq!(
            error.to_string(),
            "unknownMethod: The host does not answer meeting.setTab."
        );
    }

    #[test]
    fn request_without_params_omits_the_key_and_accepts_null() {
        let request = BridgeRequest::new("req-1", BridgeMethod::RecordingStop, None);
        assert_eq!(
            json::to_compact_string(&request).unwrap(),
            r#"{"id":"req-1","method":"recording.stop"}"#
        );
        let with_null: BridgeRequest =
            serde_json::from_str(r#"{"id":"req-1","method":"recording.stop","params":null}"#)
                .unwrap();
        assert_eq!(with_null, request);
    }

    #[test]
    fn reply_sets_at_most_one_side() {
        assert_eq!(
            json::to_compact_string(&BridgeReply::empty("r")).unwrap(),
            r#"{"id":"r"}"#
        );
        assert_eq!(
            json::to_compact_string(&BridgeReply::result("r", json!({"confirmed": true}))).unwrap(),
            r#"{"id":"r","result":{"confirmed":true}}"#
        );
        assert_eq!(
            json::to_compact_string(&BridgeReply::error(
                "r",
                BridgeError::not_found("No meeting.")
            ))
            .unwrap(),
            r#"{"error":{"code":"notFound","message":"No meeting."},"id":"r"}"#
        );
        let decoded: BridgeReply =
            serde_json::from_str(r#"{"id":"r","error":{"code":"cancelled","message":"m"}}"#)
                .unwrap();
        assert_eq!(decoded.error.unwrap().code, BridgeErrorCode::Cancelled);
    }

    #[test]
    fn event_wraps_a_snapshot_payload() {
        let event = BridgeEvent::snapshot(BridgeTopic::App, &json!({"version": "1"})).unwrap();
        assert_eq!(
            json::to_compact_string(&event).unwrap(),
            r#"{"payload":{"version":"1"},"topic":"app"}"#
        );
    }
}
