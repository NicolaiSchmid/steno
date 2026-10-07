import Foundation
import StenoCore

/// One snapshot per Settings section. Each carries the section's subtitle for
/// the Settings sidebar so the page never derives status text itself.

public struct GeneralSettingsSnapshot: Codable, Sendable, Equatable {
  public enum LoginItem: String, Codable, Sendable, CaseIterable {
    case notRegistered
    case enabled
    case requiresApproval
    case notFound
  }

  public struct Updates: Codable, Sendable, Equatable {
    public enum Outcome: String, Codable, Sendable, CaseIterable {
      case notChecked
      case upToDate
      case available
      case failed
    }

    public var canCheck: Bool
    public var automaticallyChecks: Bool
    public var automaticallyDownloads: Bool
    public var lastCheckAt: Date?
    public var outcome: Outcome
    public var detail: String?

    public init(
      canCheck: Bool, automaticallyChecks: Bool, automaticallyDownloads: Bool, lastCheckAt: Date?,
      outcome: Outcome, detail: String? = nil
    ) {
      self.canCheck = canCheck
      self.automaticallyChecks = automaticallyChecks
      self.automaticallyDownloads = automaticallyDownloads
      self.lastCheckAt = lastCheckAt
      self.outcome = outcome
      self.detail = detail
    }
  }

  /// A summary template with the sentence shown under the picker.
  public struct Template: Codable, Sendable, Equatable {
    public var id: String
    public var name: String
    public var description: String

    public init(id: String, name: String, description: String) {
      self.id = id
      self.name = name
      self.description = description
    }
  }

  /// One line of the Acknowledgements dialog: a speech model or a library,
  /// its licence and where it comes from (a URL for libraries, a repository
  /// path for models).
  public struct Acknowledgement: Codable, Sendable, Equatable {
    public enum Group: String, Codable, Sendable, CaseIterable {
      case speechModels
      case libraries
    }

    public var group: Group
    public var name: String
    public var licence: String
    public var source: String

    public init(group: Group, name: String, licence: String, source: String) {
      self.group = group
      self.name = name
      self.licence = licence
      self.source = source
    }
  }

  public var subtitle: String
  public var version: String
  public var loginItem: LoginItem
  public var detectionEnabled: Bool
  public var defaultTemplateID: String
  public var templates: [Template]
  public var calendarPermission: BridgePermissionState
  public var requestingCalendar: Bool
  public var updates: Updates
  public var acknowledgements: [Acknowledgement]
  public var error: String?
  public var errorDetails: String?

  public init(
    subtitle: String, version: String, loginItem: LoginItem, detectionEnabled: Bool,
    defaultTemplateID: String, templates: [Template],
    calendarPermission: BridgePermissionState, requestingCalendar: Bool, updates: Updates,
    acknowledgements: [Acknowledgement] = [], error: String? = nil, errorDetails: String? = nil
  ) {
    self.subtitle = subtitle
    self.version = version
    self.loginItem = loginItem
    self.detectionEnabled = detectionEnabled
    self.defaultTemplateID = defaultTemplateID
    self.templates = templates
    self.calendarPermission = calendarPermission
    self.requestingCalendar = requestingCalendar
    self.updates = updates
    self.acknowledgements = acknowledgements
    self.error = error
    self.errorDetails = errorDetails
  }
}

public struct RecordingSettingsSnapshot: Codable, Sendable, Equatable {
  public struct Device: Codable, Sendable, Equatable {
    public var uid: String
    public var name: String

    public init(uid: String, name: String) {
      self.uid = uid
      self.name = name
    }
  }

  public struct Retention: Codable, Sendable, Equatable {
    public typealias Mode = AudioRetention.Kind

    public var mode: Mode
    public var days: Int

    public init(mode: Mode, days: Int) {
      self.mode = mode
      self.days = days
    }
  }

  public struct Permission: Codable, Sendable, Equatable {
    public var kind: BridgePermissionKind
    public var state: BridgePermissionState
    public var isRequesting: Bool

    public init(kind: BridgePermissionKind, state: BridgePermissionState, isRequesting: Bool) {
      self.kind = kind
      self.state = state
      self.isRequesting = isRequesting
    }
  }

  /// Whether `folderUsageBytes` is a figure, still being measured, or could
  /// not be measured.
  public enum FolderUsage: String, Codable, Sendable, CaseIterable {
    case measuring
    case measured
    case unavailable
  }

  public var subtitle: String
  public var devices: [Device]
  /// nil means the system default input.
  public var inputDeviceUID: String?
  public var audioFolderPath: String
  public var audioFolderName: String
  public var folderUsage: FolderUsage
  public var folderUsageBytes: Int64?
  public var retention: Retention
  /// The sentence under the retention picker for the current rule.
  public var retentionFootnote: String
  public var keptForeverCount: Int?
  public var permissions: [Permission]
  public var error: String?
  public var errorDetails: String?

  public init(
    subtitle: String, devices: [Device], inputDeviceUID: String?, audioFolderPath: String,
    audioFolderName: String, folderUsage: FolderUsage = .measured, folderUsageBytes: Int64?,
    retention: Retention, retentionFootnote: String = "", keptForeverCount: Int?,
    permissions: [Permission], error: String? = nil, errorDetails: String? = nil
  ) {
    self.subtitle = subtitle
    self.devices = devices
    self.inputDeviceUID = inputDeviceUID
    self.audioFolderPath = audioFolderPath
    self.audioFolderName = audioFolderName
    self.folderUsage = folderUsage
    self.folderUsageBytes = folderUsageBytes
    self.retention = retention
    self.retentionFootnote = retentionFootnote
    self.keptForeverCount = keptForeverCount
    self.permissions = permissions
    self.error = error
    self.errorDetails = errorDetails
  }
}

public struct TranscriptionSettingsSnapshot: Codable, Sendable, Equatable {
  public struct Engine: Codable, Sendable, Equatable {
    public var id: String
    public var name: String

    public init(id: String, name: String) {
      self.id = id
      self.name = name
    }
  }

  public struct Asset: Codable, Sendable, Equatable {
    public enum State: String, Codable, Sendable, CaseIterable {
      case absent
      case downloading
      case installed
      case failed
    }

    public var id: String
    public var name: String
    public var detail: String
    public var state: State
    public var downloadFraction: Double?
    public var downloadPhase: String?
    public var installedBytes: Int64?
    public var failure: String?

    public init(
      id: String, name: String, detail: String, state: State, downloadFraction: Double? = nil,
      downloadPhase: String? = nil, installedBytes: Int64? = nil, failure: String? = nil
    ) {
      self.id = id
      self.name = name
      self.detail = detail
      self.state = state
      self.downloadFraction = downloadFraction
      self.downloadPhase = downloadPhase
      self.installedBytes = installedBytes
      self.failure = failure
    }
  }

  public var subtitle: String
  public var engineID: String
  public var engines: [Engine]
  public var showsEnginePicker: Bool
  public var assets: [Asset]
  public var allInstalled: Bool
  public var error: String?
  public var errorDetails: String?

  public init(
    subtitle: String, engineID: String, engines: [Engine], showsEnginePicker: Bool,
    assets: [Asset], allInstalled: Bool, error: String? = nil, errorDetails: String? = nil
  ) {
    self.subtitle = subtitle
    self.engineID = engineID
    self.engines = engines
    self.showsEnginePicker = showsEnginePicker
    self.assets = assets
    self.allInstalled = allInstalled
    self.error = error
    self.errorDetails = errorDetails
  }
}

public struct SummariesSettingsSnapshot: Codable, Sendable, Equatable {
  public struct Preset: Codable, Sendable, Equatable {
    public var id: String
    public var title: String
    public var needsAPIKey: Bool
    public var showsServerField: Bool
    public var modelPlaceholder: String

    public init(
      id: String, title: String, needsAPIKey: Bool, showsServerField: Bool, modelPlaceholder: String
    ) {
      self.id = id
      self.title = title
      self.needsAPIKey = needsAPIKey
      self.showsServerField = showsServerField
      self.modelPlaceholder = modelPlaceholder
    }
  }

  public struct TestResult: Codable, Sendable, Equatable {
    public var ok: Bool
    public var message: String

    public init(ok: Bool, message: String) {
      self.ok = ok
      self.message = message
    }
  }

  /// The ChatGPT (Codex) preset: what the sign-in on this Mac looks like,
  /// whether the user has confirmed its use, and the model list once
  /// confirmed. Present only while that preset is selected.
  public struct Codex: Codable, Sendable, Equatable {
    public enum SignIn: String, Codable, Sendable, CaseIterable {
      case notChecked
      case signedIn
      case unavailable
    }

    public struct Model: Codable, Sendable, Equatable {
      public var slug: String
      public var name: String

      public init(slug: String, name: String) {
        self.slug = slug
        self.name = name
      }
    }

    public var confirmed: Bool
    public var signIn: SignIn
    /// The account line while signed in; the reason while unavailable.
    public var signInDetail: String?
    /// The picked model slug; empty until the list arrives or the user picks.
    public var model: String
    public var models: [Model]
    public var isLoadingModels: Bool
    public var modelsError: String?

    public init(
      confirmed: Bool, signIn: SignIn, signInDetail: String? = nil, model: String,
      models: [Model] = [], isLoadingModels: Bool = false, modelsError: String? = nil
    ) {
      self.confirmed = confirmed
      self.signIn = signIn
      self.signInDetail = signInDetail
      self.model = model
      self.models = models
      self.isLoadingModels = isLoadingModels
      self.modelsError = modelsError
    }
  }

  public var subtitle: String
  public var presets: [Preset]
  public var presetID: String
  public var baseURL: String
  public var model: String
  public var contextTokens: String
  public var defaultContextTokens: Int
  public var hasAPIKey: Bool
  public var isConfigured: Bool
  public var isTesting: Bool
  public var testResult: TestResult?
  public var validationMessage: String?
  public var codex: Codex?
  public var error: String?
  public var errorDetails: String?
  /// Where a saved key lives; the Rust host says, the Swift app (whose key
  /// is always in the Keychain) leaves it out.
  public var keyStore: KeyStore?

  /// Where the host keeps a saved API key: the platform keyring, or the
  /// owner-only secrets file (Linux without a keyring).
  public enum KeyStore: String, Codable, Sendable, CaseIterable {
    case keyring
    case file
  }

  public init(
    subtitle: String, presets: [Preset], presetID: String, baseURL: String, model: String,
    contextTokens: String, defaultContextTokens: Int = 32_000, hasAPIKey: Bool,
    isConfigured: Bool, isTesting: Bool, testResult: TestResult? = nil,
    validationMessage: String? = nil, codex: Codex? = nil, error: String? = nil,
    errorDetails: String? = nil, keyStore: KeyStore? = nil
  ) {
    self.subtitle = subtitle
    self.presets = presets
    self.presetID = presetID
    self.baseURL = baseURL
    self.model = model
    self.contextTokens = contextTokens
    self.defaultContextTokens = defaultContextTokens
    self.hasAPIKey = hasAPIKey
    self.isConfigured = isConfigured
    self.isTesting = isTesting
    self.testResult = testResult
    self.validationMessage = validationMessage
    self.codex = codex
    self.error = error
    self.errorDetails = errorDetails
    self.keyStore = keyStore
  }
}

public struct ExportSettingsSnapshot: Codable, Sendable, Equatable {
  public var subtitle: String
  public var enabled: Bool
  public var vaultPath: String?
  public var vaultName: String?
  public var peopleFolder: String
  public var includeAudio: Bool
  public var taskTag: String
  public var validationMessage: String?
  public var saved: Bool
  public var error: String?
  public var errorDetails: String?

  public init(
    subtitle: String, enabled: Bool, vaultPath: String?, vaultName: String?, peopleFolder: String,
    includeAudio: Bool, taskTag: String, validationMessage: String? = nil, saved: Bool,
    error: String? = nil, errorDetails: String? = nil
  ) {
    self.subtitle = subtitle
    self.enabled = enabled
    self.vaultPath = vaultPath
    self.vaultName = vaultName
    self.peopleFolder = peopleFolder
    self.includeAudio = includeAudio
    self.taskTag = taskTag
    self.validationMessage = validationMessage
    self.saved = saved
    self.error = error
    self.errorDetails = errorDetails
  }
}

public struct PhoneSettingsSnapshot: Codable, Sendable, Equatable {
  public struct Device: Codable, Sendable, Equatable {
    public var id: UUID
    public var name: String
    public var pairedAt: Date
    public var lastSeenAt: Date?

    public init(id: UUID, name: String, pairedAt: Date, lastSeenAt: Date?) {
      self.id = id
      self.name = name
      self.pairedAt = pairedAt
      self.lastSeenAt = lastSeenAt
    }
  }

  public struct Listener: Codable, Sendable, Equatable {
    public enum State: String, Codable, Sendable, CaseIterable {
      case unavailable
      case stopped
      case starting
      case listening
      case failed
    }

    public var state: State
    public var port: Int?
    public var failure: String?

    public init(state: State, port: Int? = nil, failure: String? = nil) {
      self.state = state
      self.port = port
      self.failure = failure
    }
  }

  public struct Pairing: Codable, Sendable, Equatable {
    public var expiresAt: Date
    /// The QR code as a base64 PNG; the page draws it in an `img`.
    public var qrPNGBase64: String

    public init(expiresAt: Date, qrPNGBase64: String) {
      self.expiresAt = expiresAt
      self.qrPNGBase64 = qrPNGBase64
    }
  }

  public struct Receipt: Codable, Sendable, Equatable {
    public var deviceID: UUID
    public var recordingID: UUID
    public var receivedBytes: Int64
    public var totalBytes: Int64?

    public init(deviceID: UUID, recordingID: UUID, receivedBytes: Int64, totalBytes: Int64?) {
      self.deviceID = deviceID
      self.recordingID = recordingID
      self.receivedBytes = receivedBytes
      self.totalBytes = totalBytes
    }
  }

  public var subtitle: String
  public var macID: String?
  public var devices: [Device]
  public var listener: Listener
  public var pairing: Pairing?
  public var receipts: [Receipt]
  public var error: String?
  public var errorDetails: String?

  public init(
    subtitle: String, macID: String?, devices: [Device], listener: Listener,
    pairing: Pairing? = nil,
    receipts: [Receipt], error: String? = nil, errorDetails: String? = nil
  ) {
    self.subtitle = subtitle
    self.macID = macID
    self.devices = devices
    self.listener = listener
    self.pairing = pairing
    self.receipts = receipts
    self.error = error
    self.errorDetails = errorDetails
  }
}
