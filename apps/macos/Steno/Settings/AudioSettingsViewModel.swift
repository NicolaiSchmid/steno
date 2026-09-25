import Foundation
import StenoAudio
import StenoCore

/// Audio: input device, recordings folder, default retention.
@MainActor
@Observable
final class AudioSettingsViewModel {
  enum RetentionMode: String, CaseIterable, Identifiable, Sendable {
    case deleteAfterProcessing
    case keepDays
    case keepForever

    var id: String { rawValue }

    var title: String {
      switch self {
      case .deleteAfterProcessing: "Delete after processing"
      case .keepDays: "Keep for a number of days"
      case .keepForever: "Keep forever"
      }
    }
  }

  private(set) var devices: [AudioDeviceInfo] = []
  private(set) var inputDeviceUID: String?
  private(set) var audioFolder: URL = Settings.defaultAudioFolder
  private(set) var retentionMode: RetentionMode = .keepDays
  private(set) var retentionDays = 30
  private(set) var error: String?
  private let environment: AppEnvironment
  /// Lists the input devices; the live one asks Core Audio.
  var listInputs: @Sendable () throws -> [AudioDeviceInfo] = { try AudioDevices.inputs() }

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  func load() async {
    do {
      let settings = try await environment.settings.load()
      inputDeviceUID = settings.inputDeviceUID
      audioFolder = settings.audioFolder
      switch settings.defaultRetention {
      case .deleteAfterProcessing: retentionMode = .deleteAfterProcessing
      case .keepDays(let days):
        retentionMode = .keepDays
        retentionDays = days
      case .keepForever: retentionMode = .keepForever
      }
    } catch {
      self.error = "Settings could not be loaded: \(error)"
    }
    refreshDevices()
  }

  func refreshDevices() {
    do {
      devices = try listInputs()
    } catch {
      devices = []
      self.error = "Input devices could not be listed: \(error)"
    }
  }

  var retention: AudioRetention {
    switch retentionMode {
    case .deleteAfterProcessing: .deleteAfterProcessing
    case .keepDays: .keepDays(max(1, retentionDays))
    case .keepForever: .keepForever
    }
  }

  func setInputDevice(_ uid: String?) async {
    inputDeviceUID = uid
    await save { $0.inputDeviceUID = uid }
  }

  func setAudioFolder(_ url: URL) async {
    audioFolder = url
    await save { $0.audioFolder = url }
  }

  func setRetention(mode: RetentionMode, days: Int) async {
    retentionMode = mode
    retentionDays = max(1, days)
    let retention = self.retention
    await save { $0.defaultRetention = retention }
  }

  private func save(_ mutate: (inout Settings) -> Void) async {
    do {
      var settings = try await environment.settings.load()
      mutate(&settings)
      try await environment.settings.save(settings)
      error = nil
    } catch {
      self.error = "Setting could not be saved: \(error)"
    }
  }
}
