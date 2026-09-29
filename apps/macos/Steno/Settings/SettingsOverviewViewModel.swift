import Foundation
import StenoCore
import StenoLLM
import StenoSpeech

/// The sidebar subtitles: one short status per section, computed from
/// `Settings`, the permissions, the model store, the paired devices and the
/// updater. Refreshed on appear and whenever the selection changes.
@MainActor
@Observable
final class SettingsOverviewViewModel {
  private(set) var subtitles: [SettingsSection: String] = [:]
  private let environment: AppEnvironment

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  func refresh() async {
    let settings = try? await environment.settings.load()
    let microphone = await environment.permissions.state(of: .microphone)
    let systemAudio = await environment.permissions.state(of: .systemAudio)
    let engine =
      settings.flatMap { try? SpeechEngineID(settingsValue: $0.speechEngineID) } ?? .parakeetV3
    let modelsInstalled =
      environment.models.isInstalled(engine.asset)
      && environment.models.isInstalled(.offlineDiarizer)
    var pairedCount = 0
    if let handover = environment.handover {
      pairedCount = (try? await handover.pairedDevices())?.count ?? 0
    }
    subtitles = Self.subtitles(
      settings: settings,
      recordingReady: microphone == .granted && systemAudio == .granted,
      modelsInstalled: modelsInstalled,
      pairedCount: pairedCount,
      handoverAvailable: environment.handover != nil,
      updateOutcome: environment.updater.lastOutcome,
      version: AppVersion.marketing)
  }

  nonisolated static func subtitles(
    settings: Settings?,
    recordingReady: Bool,
    modelsInstalled: Bool,
    pairedCount: Int,
    handoverAvailable: Bool,
    updateOutcome: UpdateCheckOutcome,
    version: String
  ) -> [SettingsSection: String] {
    var result: [SettingsSection: String] = [:]
    result[.general] =
      switch updateOutcome {
      case .available(let available): "Update available: \(available)"
      case .failed: "Update check failed"
      case .upToDate, .notChecked: "Steno \(version)"
      }
    result[.recording] = recordingReady ? "Ready" : "Permission needed"
    result[.transcription] = modelsInstalled ? "Ready" : "Download needed"
    if let settings, LLMEndpoint(settings: settings) != nil {
      let preset = LLMPreset.infer(from: settings)
      result[.summaries] = preset == .custom ? (settings.llmModel ?? "Custom server") : preset.title
    } else {
      result[.summaries] = "Not set up"
    }
    if let vault = settings?.obsidian?.vaultPath, !vault.isEmpty {
      result[.export] = URL(fileURLWithPath: vault).lastPathComponent
    } else {
      result[.export] = "Off"
    }
    result[.iphone] =
      if !handoverAvailable {
        "Unavailable"
      } else if pairedCount == 0 {
        "No iPhone paired"
      } else if pairedCount == 1 {
        "1 iPhone paired"
      } else {
        "\(pairedCount) iPhones paired"
      }
    return result
  }
}
