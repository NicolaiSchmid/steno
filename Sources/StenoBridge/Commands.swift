import Foundation

/// Params of the page's method calls, one type per `BridgeMethod` that takes
/// arguments. Methods without a params type take `null`.

public struct PageLayoutParams: Codable, Sendable, Equatable {
  public var window: BridgeWindow
  public var width: Double
  public var height: Double

  public init(window: BridgeWindow, width: Double, height: Double) {
    self.window = window
    self.width = width
    self.height = height
  }
}

public struct SetFilterParams: Codable, Sendable, Equatable {
  public var filter: BridgeListFilter

  public init(filter: BridgeListFilter) { self.filter = filter }
}

public struct SetTagFilterParams: Codable, Sendable, Equatable {
  /// nil clears the tag filter.
  public var tag: String?

  public init(tag: String?) { self.tag = tag }
}

public struct SetQueryParams: Codable, Sendable, Equatable {
  public var query: String

  public init(query: String) { self.query = query }
}

public struct MeetingIDParams: Codable, Sendable, Equatable {
  public var meetingID: UUID

  public init(meetingID: UUID) { self.meetingID = meetingID }
}

public struct SetTabParams: Codable, Sendable, Equatable {
  public var tab: MeetingDetailSnapshot.Tab

  public init(tab: MeetingDetailSnapshot.Tab) { self.tab = tab }
}

public struct SetTagsParams: Codable, Sendable, Equatable {
  public var tags: [String]

  public init(tags: [String]) { self.tags = tags }
}

public struct SetTemplateParams: Codable, Sendable, Equatable {
  public var templateID: String

  public init(templateID: String) { self.templateID = templateID }
}

public struct SetBoolParams: Codable, Sendable, Equatable {
  public var value: Bool

  public init(value: Bool) { self.value = value }
}

public struct SetStringParams: Codable, Sendable, Equatable {
  public var value: String

  public init(value: String) { self.value = value }
}

/// Notes carry their meeting so a save that arrives after the selection
/// moved on lands on the meeting it was typed for.
public struct SaveNotesParams: Codable, Sendable, Equatable {
  public var meetingID: UUID
  public var text: String

  public init(meetingID: UUID, text: String) {
    self.meetingID = meetingID
    self.text = text
  }
}

/// `speakers.options` reply: what the speaker picker offers for one speaker.
public struct SpeakerOptionsParams: Codable, Sendable, Equatable {
  public var speakerID: UUID
  public var query: String

  public init(speakerID: UUID, query: String) {
    self.speakerID = speakerID
    self.query = query
  }
}

public struct SpeakerOption: Codable, Sendable, Equatable {
  public enum Kind: String, Codable, Sendable, CaseIterable {
    /// An existing person; `personID` is set.
    case person
    /// Create a person named `label`.
    case create
    /// Mark the speaker as unknown.
    case unknown
  }

  public var kind: Kind
  public var label: String
  public var detail: String?
  public var personID: UUID?

  public init(kind: Kind, label: String, detail: String? = nil, personID: UUID? = nil) {
    self.kind = kind
    self.label = label
    self.detail = detail
    self.personID = personID
  }
}

public struct SpeakerOptionsReply: Codable, Sendable, Equatable {
  public var prefill: String?
  public var options: [SpeakerOption]

  public init(prefill: String?, options: [SpeakerOption]) {
    self.prefill = prefill
    self.options = options
  }
}

public struct SelectSpeakerParams: Codable, Sendable, Equatable {
  public var speakerID: UUID
  public var option: SpeakerOption

  public init(speakerID: UUID, option: SpeakerOption) {
    self.speakerID = speakerID
    self.option = option
  }
}

public struct SpeakerIDParams: Codable, Sendable, Equatable {
  public var speakerID: UUID

  public init(speakerID: UUID) { self.speakerID = speakerID }
}

public struct StartRecordingParams: Codable, Sendable, Equatable {
  public var mode: BridgeCaptureMode

  public init(mode: BridgeCaptureMode) { self.mode = mode }
}

public struct SetRetentionParams: Codable, Sendable, Equatable {
  public var retention: RecordingSettingsSnapshot.Retention

  public init(retention: RecordingSettingsSnapshot.Retention) { self.retention = retention }
}

public struct PermissionKindParams: Codable, Sendable, Equatable {
  public var kind: BridgePermissionKind

  public init(kind: BridgePermissionKind) { self.kind = kind }
}

public struct AssetIDParams: Codable, Sendable, Equatable {
  public var assetID: String

  public init(assetID: String) { self.assetID = assetID }
}

public struct SetAutomaticUpdatesParams: Codable, Sendable, Equatable {
  public var automaticallyChecks: Bool
  public var automaticallyDownloads: Bool

  public init(automaticallyChecks: Bool, automaticallyDownloads: Bool) {
    self.automaticallyChecks = automaticallyChecks
    self.automaticallyDownloads = automaticallyDownloads
  }
}

/// Partial update of the Summaries form; nil fields are left alone.
public struct SummariesUpdateParams: Codable, Sendable, Equatable {
  public var baseURL: String?
  public var model: String?
  public var contextTokens: String?
  public var apiKey: String?

  public init(
    baseURL: String? = nil, model: String? = nil, contextTokens: String? = nil,
    apiKey: String? = nil
  ) {
    self.baseURL = baseURL
    self.model = model
    self.contextTokens = contextTokens
    self.apiKey = apiKey
  }
}

/// Partial update of the Export form; nil fields are left alone.
public struct ExportUpdateParams: Codable, Sendable, Equatable {
  public var peopleFolder: String?
  public var includeAudio: Bool?
  public var taskTag: String?

  public init(peopleFolder: String? = nil, includeAudio: Bool? = nil, taskTag: String? = nil) {
    self.peopleFolder = peopleFolder
    self.includeAudio = includeAudio
    self.taskTag = taskTag
  }
}

public struct DeviceIDParams: Codable, Sendable, Equatable {
  public var deviceID: UUID

  public init(deviceID: UUID) { self.deviceID = deviceID }
}

public struct SetupStepParams: Codable, Sendable, Equatable {
  public var step: OnboardingSnapshot.SetupStep.Kind

  public init(step: OnboardingSnapshot.SetupStep.Kind) { self.step = step }
}

public struct OpenURLParams: Codable, Sendable, Equatable {
  public var url: String

  public init(url: String) { self.url = url }
}

/// `settings.showSection`: the section the page has just shown.
public struct ShowSectionParams: Codable, Sendable, Equatable {
  public var section: BridgeSettingsSection

  public init(section: BridgeSettingsSection) { self.section = section }
}

public struct WindowParams: Codable, Sendable, Equatable {
  public var window: BridgeWindow
  public var section: BridgeSettingsSection?
  public var meetingID: UUID?

  public init(window: BridgeWindow, section: BridgeSettingsSection? = nil, meetingID: UUID? = nil) {
    self.window = window
    self.section = section
    self.meetingID = meetingID
  }
}

/// `ui.confirmDestructive`: the host shows an `NSAlert`; the reply says
/// whether the user confirmed.
public struct ConfirmDestructiveParams: Codable, Sendable, Equatable {
  public var title: String
  public var message: String
  public var confirmTitle: String

  public init(title: String, message: String, confirmTitle: String) {
    self.title = title
    self.message = message
    self.confirmTitle = confirmTitle
  }
}

public struct ConfirmReply: Codable, Sendable, Equatable {
  public var confirmed: Bool

  public init(confirmed: Bool) { self.confirmed = confirmed }
}

/// `settings.recording.chooseFolder` and `settings.export.chooseVault`
/// return the chosen path, or nil when the panel was cancelled.
public struct ChosenPathReply: Codable, Sendable, Equatable {
  public var path: String?

  public init(path: String?) { self.path = path }
}
