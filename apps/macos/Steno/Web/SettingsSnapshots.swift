import Foundation
import StenoBridge
import StenoCore
import StenoHandover
import StenoLLM
import StenoSpeech

// The Settings window's topics as pure mappings from the six section view
// models to the contract snapshots (plan Decision 6). One initializer per
// section; each reads only its own view model and the sidebar subtitle, so
// `SettingsBridge`'s observation tracking re-publishes a section exactly
// when its inputs change. No SwiftUI and no store access here: the view
// models own every rule, these spell their state in the wire vocabulary.

/// The helpers the mappings share.
enum SettingsSnapshots {
  static func kind(_ kind: PermissionKind) -> BridgePermissionKind {
    switch kind {
    case .microphone: .microphone
    case .systemAudio: .systemAudio
    case .calendar: .calendar
    case .localNetwork: .localNetwork
    }
  }

  static func state(_ state: PermissionState) -> BridgePermissionState {
    switch state {
    case .unknown: .unknown
    case .granted: .granted
    case .denied: .denied
    }
  }

  static func loginItem(_ status: LoginItemStatus) -> GeneralSettingsSnapshot.LoginItem {
    switch status {
    case .notRegistered: .notRegistered
    case .enabled: .enabled
    case .requiresApproval: .requiresApproval
    case .notFound: .notFound
    }
  }

  /// The libraries Steno ships with, for the Acknowledgements dialog. The
  /// speech models come from `ModelAsset` so the list follows the code.
  static let libraries: [GeneralSettingsSnapshot.Acknowledgement] = [
    .init(
      group: .libraries, name: "Sparkle", licence: "MIT",
      source: "https://github.com/sparkle-project/Sparkle"),
    .init(
      group: .libraries, name: "GRDB.swift", licence: "MIT",
      source: "https://github.com/groue/GRDB.swift"),
    .init(
      group: .libraries, name: "FluidAudio", licence: "Apache-2.0",
      source: "https://github.com/FluidInference/FluidAudio"),
    .init(
      group: .libraries, name: "WhisperKit", licence: "MIT",
      source: "https://github.com/argmaxinc/WhisperKit"),
    .init(
      group: .libraries, name: "SwiftNIO", licence: "Apache-2.0",
      source: "https://github.com/apple/swift-nio"),
    .init(
      group: .libraries, name: "Swift Crypto", licence: "Apache-2.0",
      source: "https://github.com/apple/swift-crypto"),
    .init(
      group: .libraries, name: "Speex", licence: "BSD",
      source: "https://github.com/sbooth/CSpeex"),
  ]

  static var acknowledgements: [GeneralSettingsSnapshot.Acknowledgement] {
    ModelAsset.allCases.map {
      GeneralSettingsSnapshot.Acknowledgement(
        group: .speechModels, name: $0.displayName, licence: $0.licence, source: $0.sourceRepo)
    } + libraries
  }

  /// Bytes received so far: whole chunks, never past the declared size.
  static func receivedBytes(_ receipt: HandoverReceipt) -> Int64 {
    let received = Int64(receipt.receivedChunks.count) * Int64(receipt.chunkSize)
    return min(received, receipt.byteCount)
  }
}

// MARK: - settings.general

extension GeneralSettingsSnapshot {
  @MainActor
  init(general: GeneralSettingsViewModel, subtitle: String) {
    let outcome: Updates.Outcome
    var detail: String?
    switch general.updateOutcome {
    case .notChecked:
      outcome = .notChecked
    case .upToDate:
      outcome = .upToDate
    case .available(let version):
      outcome = .available
      detail = version
    case .failed(let message):
      outcome = .failed
      detail = message
    }
    self.init(
      subtitle: subtitle, version: general.version,
      loginItem: SettingsSnapshots.loginItem(general.loginItem),
      detectionEnabled: general.detectionEnabled, defaultTemplateID: general.defaultTemplateID,
      templates: general.templates.map {
        Template(id: $0.id, name: $0.displayName, description: $0.description)
      },
      calendarPermission: SettingsSnapshots.state(general.calendarPermission),
      requestingCalendar: general.requestingCalendar,
      updates: Updates(
        canCheck: general.canCheckForUpdates,
        automaticallyChecks: general.automaticallyChecksForUpdates,
        automaticallyDownloads: general.automaticallyDownloadsUpdates,
        lastCheckAt: general.lastUpdateCheckDate, outcome: outcome, detail: detail),
      acknowledgements: SettingsSnapshots.acknowledgements,
      error: general.error, errorDetails: general.errorDetails)
  }
}

// MARK: - settings.recording

extension RecordingSettingsSnapshot {
  @MainActor
  init(audio: AudioSettingsViewModel, subtitle: String) {
    let usage: FolderUsage
    var bytes: Int64?
    switch audio.folderUsage {
    case .measuring:
      usage = .measuring
    case .bytes(let measured):
      usage = .measured
      bytes = measured
    case .unavailable:
      usage = .unavailable
    }
    let mode: Retention.Mode =
      switch audio.retentionMode {
      case .keepForever: .keepForever
      case .keepDays: .keepDays
      case .deleteAfterProcessing: .deleteAfterProcessing
      }
    self.init(
      subtitle: subtitle,
      devices: audio.devices.map { Device(uid: $0.uid, name: $0.name) },
      inputDeviceUID: audio.inputDeviceUID, audioFolderPath: audio.audioFolder.path,
      audioFolderName: audio.folderName, folderUsage: usage, folderUsageBytes: bytes,
      retention: Retention(mode: mode, days: audio.retentionDays),
      retentionFootnote: audio.footnote, keptForeverCount: audio.keptForever,
      permissions: AudioSettingsViewModel.recordingPermissions.map { kind in
        Permission(
          kind: SettingsSnapshots.kind(kind), state: SettingsSnapshots.state(audio.state(of: kind)),
          isRequesting: audio.requesting == kind)
      },
      error: audio.error, errorDetails: audio.errorDetails)
  }
}

// MARK: - settings.transcription

extension TranscriptionSettingsSnapshot {
  @MainActor
  init(speech: SpeechSettingsViewModel, subtitle: String) {
    self.init(
      subtitle: subtitle, engineID: speech.engineID.rawValue,
      engines: speech.engines.map {
        Engine(id: $0.rawValue, name: SpeechSettingsViewModel.engineTitle($0))
      },
      showsEnginePicker: speech.showsEnginePicker,
      assets: speech.assets.map { asset in
        var row = Asset(
          id: asset.rawValue, name: SpeechSettingsViewModel.componentTitle(asset),
          detail: speech.statusText(of: asset), state: .absent)
        switch speech.state(of: asset) {
        case .absent:
          row.state = .absent
        case .downloading(let fraction, let phase):
          row.state = .downloading
          row.downloadFraction = fraction
          row.downloadPhase = phase
        case .installed(let bytes):
          row.state = .installed
          row.installedBytes = bytes ?? asset.approximateBytes
        case .failed(let message):
          row.state = .failed
          row.failure = message
        }
        return row
      },
      allInstalled: speech.allInstalled, error: speech.error, errorDetails: speech.errorDetails)
  }
}

// MARK: - settings.summaries

extension SummariesSettingsSnapshot {
  @MainActor
  init(llm: LLMSettingsViewModel, subtitle: String) {
    var result: TestResult?
    switch llm.testResult {
    case .success(let message): result = TestResult(ok: true, message: message)
    case .failure(let message): result = TestResult(ok: false, message: message)
    case nil: break
    }
    var codex: Codex?
    if llm.preset == .codex {
      let signIn: Codex.SignIn
      var detail: String?
      switch llm.codexStatus {
      case .notChecked:
        signIn = .notChecked
      case .signedIn(let account):
        signIn = .signedIn
        detail = account
      case .unavailable(let reason):
        signIn = .unavailable
        detail = reason
      }
      codex = Codex(
        confirmed: llm.codexConfirmed, signIn: signIn, signInDetail: detail, model: llm.codexModel,
        models: llm.codexModelChoices.map { Codex.Model(slug: $0.slug, name: $0.displayName) },
        isLoadingModels: llm.isLoadingCodexModels, modelsError: llm.codexModelsError)
    }
    self.init(
      subtitle: subtitle,
      presets: LLMPreset.allCases.map {
        Preset(
          id: $0.rawValue, title: $0.title, needsAPIKey: $0.needsAPIKey,
          showsServerField: $0.showsServerField, modelPlaceholder: $0.modelPlaceholder)
      },
      presetID: llm.preset.rawValue, baseURL: llm.baseURLText, model: llm.model,
      contextTokens: llm.contextTokensText,
      defaultContextTokens: LLMSettingsViewModel.defaultContextTokens,
      // Whether a key is stored, never the key: the page shows a placeholder.
      hasAPIKey: llm.hasStoredAPIKey, isConfigured: llm.isConfigured, isTesting: llm.isTesting,
      testResult: result, validationMessage: llm.validationMessage, codex: codex,
      error: llm.error, errorDetails: llm.errorDetails)
  }
}

// MARK: - settings.export

extension ExportSettingsSnapshot {
  @MainActor
  init(obsidian: ObsidianSettingsViewModel, subtitle: String) {
    let vault = obsidian.vaultURL
    self.init(
      subtitle: subtitle, enabled: obsidian.enabled, vaultPath: vault?.path,
      vaultName: vault == nil ? nil : obsidian.vaultName, peopleFolder: obsidian.peopleFolder,
      includeAudio: obsidian.includeAudio, taskTag: obsidian.taskTag,
      validationMessage: obsidian.validationMessage, saved: obsidian.saved,
      error: obsidian.error, errorDetails: obsidian.errorDetails)
  }
}

// MARK: - settings.iphone

extension PhoneSettingsSnapshot {
  @MainActor
  init(phones: PhonesSettingsViewModel, subtitle: String) {
    let listener: Listener
    if !phones.isAvailable {
      listener = Listener(state: .unavailable)
    } else {
      switch phones.listener {
      case .stopped: listener = Listener(state: .stopped)
      case .listening(let port): listener = Listener(state: .listening, port: Int(port))
      case .failed(let message): listener = Listener(state: .failed, failure: message)
      }
    }
    var pairing: Pairing?
    if let payload = phones.pairing, let png = phones.qrPNG {
      pairing = Pairing(expiresAt: payload.expiresAt, qrPNGBase64: png.base64EncodedString())
    }
    self.init(
      subtitle: subtitle, macID: phones.macID,
      devices: phones.devices.map {
        Device(id: $0.id, name: $0.name, pairedAt: $0.pairedAt, lastSeenAt: $0.lastSeenAt)
      },
      listener: listener, pairing: pairing,
      receipts: phones.activeReceipts.map {
        Receipt(
          deviceID: $0.deviceID, recordingID: $0.recordingID,
          receivedBytes: SettingsSnapshots.receivedBytes($0), totalBytes: $0.byteCount)
      },
      error: phones.error, errorDetails: phones.errorDetails)
  }
}
