import Foundation
import StenoCore

/// The JSON contract between the Swift host and the web UI
/// (`apps/macos/web/src/bridge/contract.ts`). Swift is the source of truth:
/// every type here is encoded into `apps/macos/web/fixtures/bridge/` by
/// `BridgeFixturesTests` and the web tests parse the same files, so a rename
/// on either side fails one CI or the other. Snapshots flow host to page as
/// events; commands flow page to host as method calls with a reply.
///
/// Plan: `.plans/2026-09-29-macos-webview-ui.md`, Decisions 5 and 6.
public enum BridgeJSON {
  /// `StenoJSON`'s convention: sorted keys, indentation, ISO dates, so the
  /// fixtures diff cleanly in git and two encodes of equal values match.
  public static func encoder() -> JSONEncoder { StenoJSON.encoder() }

  public static func decoder() -> JSONDecoder { StenoJSON.decoder() }

  public static func encode<T: Encodable>(_ value: T) throws -> Data {
    try encoder().encode(value)
  }

  public static func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
    try decoder().decode(type, from: data)
  }
}

/// Topics the host publishes. The page subscribes by name; every publish
/// carries a full snapshot of the topic, never a delta.
public enum BridgeTopic: String, Codable, Sendable, CaseIterable {
  case app
  case recording
  case progress
  case meetingsList = "meetings.list"
  case meetingDetail = "meeting.detail"
  case settingsGeneral = "settings.general"
  case settingsRecording = "settings.recording"
  case settingsTranscription = "settings.transcription"
  case settingsSummaries = "settings.summaries"
  case settingsExport = "settings.export"
  case settingsPhone = "settings.iphone"
  case onboarding
}

/// Methods the page may call. Each maps one to one onto a view model method
/// on the host; the params type is named beside it in `Commands.swift`.
public enum BridgeMethod: String, Codable, Sendable, CaseIterable {
  case pageReady = "page.ready"
  case pageLayout = "page.layout"

  case meetingsSetFilter = "meetings.setFilter"
  case meetingsSetTagFilter = "meetings.setTagFilter"
  case meetingsSetQuery = "meetings.setQuery"
  case meetingsSelect = "meetings.select"
  case meetingsDelete = "meetings.delete"

  case meetingSetTab = "meeting.setTab"
  case meetingSetTags = "meeting.setTags"
  case meetingSetTemplate = "meeting.setTemplate"
  case meetingRerunSummary = "meeting.rerunSummary"
  case meetingReexport = "meeting.reexport"
  case meetingSetKeepAudio = "meeting.setKeepAudio"
  case meetingDeleteRecordingNow = "meeting.deleteRecordingNow"
  case meetingSaveNotes = "meeting.saveNotes"
  case meetingRevealRecording = "meeting.revealRecording"
  case meetingRevealExport = "meeting.revealExport"

  case speakersOptions = "speakers.options"
  case speakersSelect = "speakers.select"
  case speakersPlay = "speakers.play"
  case speakersStop = "speakers.stop"

  case recordingStart = "recording.start"
  case recordingStop = "recording.stop"
  case recordingToggle = "recording.toggle"
  case recordingKeepGoing = "recording.keepGoing"
  case recordingClearMessages = "recording.clearMessages"

  case setupDismissBanner = "setup.dismissBanner"

  case settingsGeneralSetLaunchAtLogin = "settings.general.setLaunchAtLogin"
  case settingsGeneralSetDetection = "settings.general.setDetectionEnabled"
  case settingsGeneralSetDefaultTemplate = "settings.general.setDefaultTemplate"
  case settingsGeneralRequestCalendar = "settings.general.requestCalendar"
  case settingsGeneralSetAutomaticUpdates = "settings.general.setAutomaticUpdates"
  case settingsGeneralOpenLoginItems = "settings.general.openLoginItems"
  /// `SetStringParams`; an empty value means the system default input.
  case settingsRecordingSetInputDevice = "settings.recording.setInputDevice"
  case settingsRecordingRefreshDevices = "settings.recording.refreshDevices"
  case settingsRecordingChooseFolder = "settings.recording.chooseFolder"
  case settingsRecordingRevealFolder = "settings.recording.revealFolder"
  case settingsRecordingSetRetention = "settings.recording.setRetention"
  case settingsRecordingRequestPermission = "settings.recording.requestPermission"
  case settingsTranscriptionSetEngine = "settings.transcription.setEngine"
  case settingsTranscriptionDownload = "settings.transcription.download"
  case settingsTranscriptionRemove = "settings.transcription.remove"
  case settingsSummariesSelectPreset = "settings.summaries.selectPreset"
  case settingsSummariesUpdate = "settings.summaries.update"
  case settingsSummariesSave = "settings.summaries.save"
  case settingsSummariesTest = "settings.summaries.test"
  case settingsSummariesConfirmCodex = "settings.summaries.confirmCodex"
  case settingsSummariesRefreshCodexStatus = "settings.summaries.refreshCodexStatus"
  case settingsSummariesRefreshCodexModels = "settings.summaries.refreshCodexModels"
  /// `SetStringParams`: the model slug.
  case settingsSummariesSelectCodexModel = "settings.summaries.selectCodexModel"
  case settingsSummariesStopUsingCodex = "settings.summaries.stopUsingCodex"
  case settingsExportSetEnabled = "settings.export.setEnabled"
  case settingsExportChooseVault = "settings.export.chooseVault"
  case settingsExportUpdate = "settings.export.update"
  case settingsExportSave = "settings.export.save"
  case settingsPhoneBeginPairing = "settings.iphone.beginPairing"
  case settingsPhoneCancelPairing = "settings.iphone.cancelPairing"
  case settingsPhoneRevoke = "settings.iphone.revoke"
  /// `ShowSectionParams`: the page shows a section; the host refreshes the
  /// sidebar subtitles and clears a matching `requestedSettingsSection`.
  case settingsShowSection = "settings.showSection"

  case onboardingRequest = "onboarding.request"
  case onboardingSkip = "onboarding.skip"
  case onboardingAdvance = "onboarding.advance"
  case onboardingBack = "onboarding.back"
  case onboardingSaveSummaries = "onboarding.saveSummaries"
  case onboardingSaveVault = "onboarding.saveVault"
  case onboardingSkipSetup = "onboarding.skipSetup"
  case onboardingFinish = "onboarding.finish"

  case updatesCheck = "updates.check"
  case systemOpenURL = "system.openURL"
  case systemOpenSystemSettings = "system.openSystemSettings"
  case windowOpen = "window.open"
  case windowClose = "window.close"
  case uiConfirmDestructive = "ui.confirmDestructive"
}

/// One call from the page. `id` is echoed in the reply so the page can match
/// promises; `params` is the method's params type, or `null`.
public struct BridgeRequest: Codable, Sendable, Equatable {
  public var id: String
  public var method: BridgeMethod
  public var params: JSONValue?

  public init(id: String, method: BridgeMethod, params: JSONValue? = nil) {
    self.id = id
    self.method = method
    self.params = params
  }
}

/// The host's answer. Exactly one of `result` and `error` is set.
public struct BridgeReply: Codable, Sendable, Equatable {
  public var id: String
  public var result: JSONValue?
  public var error: BridgeError?

  public init(id: String, result: JSONValue? = nil, error: BridgeError? = nil) {
    self.id = id
    self.result = result
    self.error = error
  }
}

public struct BridgeError: Codable, Sendable, Equatable, Error {
  public enum Code: String, Codable, Sendable, CaseIterable {
    case unknownMethod
    case invalidParams
    case notFound
    case failed
    case cancelled
  }

  public var code: Code
  public var message: String

  public init(code: Code, message: String) {
    self.code = code
    self.message = message
  }
}

/// One publish from the host. The page dispatches on `topic` and decodes
/// `payload` as that topic's snapshot type.
public struct BridgeEvent: Codable, Sendable, Equatable {
  public var topic: BridgeTopic
  public var payload: JSONValue

  public init(topic: BridgeTopic, payload: JSONValue) {
    self.topic = topic
    self.payload = payload
  }
}

// MARK: - Shared vocabulary

public enum BridgeAppearance: String, Codable, Sendable, CaseIterable {
  case light
  case dark
}

public enum BridgePermissionKind: String, Codable, Sendable, CaseIterable {
  case microphone
  case systemAudio
  case calendar
  case localNetwork
}

public enum BridgePermissionState: String, Codable, Sendable, CaseIterable {
  case unknown
  case granted
  case denied
}

public enum BridgeSettingsSection: String, Codable, Sendable, CaseIterable {
  case general
  case recording
  case transcription
  case summaries
  case export
  case iphone
}

public enum BridgeWindow: String, Codable, Sendable, CaseIterable {
  case main
  case settings
  case onboarding
}

public enum BridgeMeetingSource: String, Codable, Sendable, CaseIterable {
  case call
  case inPerson
  case phone
}

public typealias BridgeMeetingState = MeetingState.Kind

public enum BridgeCaptureMode: String, Codable, Sendable, CaseIterable {
  case call
  case inPerson
}
