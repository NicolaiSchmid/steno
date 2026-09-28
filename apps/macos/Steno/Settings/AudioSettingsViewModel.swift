import Foundation
import StenoAudio
import StenoCore

/// Audio: input device, recordings folder with its disk usage, and the
/// "Keep recordings" rule. Switching the rule to Forever keeps every
/// recording still on disk (`RetentionSweep.keepAll()`); a shorter rule
/// applies to new recordings only.
@MainActor
@Observable
final class AudioSettingsViewModel {
  /// The picker's options, in display order.
  enum RetentionMode: String, CaseIterable, Identifiable, Sendable {
    case keepForever
    case keepDays
    case deleteAfterProcessing

    var id: String { rawValue }

    /// The option label; the days option shows the stored number.
    func title(days: Int) -> String {
      switch self {
      case .keepForever: "Forever"
      case .keepDays: "For \(days) \(days == 1 ? "day" : "days")"
      case .deleteAfterProcessing: "Until processed, then delete"
      }
    }
  }

  /// The recordings folder's logical size, measured off the main actor on
  /// `load()` and after the folder changes.
  enum FolderUsage: Equatable, Sendable {
    case measuring
    case bytes(Int64)
    case unavailable
  }

  static let dayRange = 1...3650

  private(set) var devices: [AudioDeviceInfo] = []
  private(set) var inputDeviceUID: String?
  private(set) var audioFolder: URL = Settings.defaultAudioFolder
  private(set) var folderUsage: FolderUsage = .measuring
  private(set) var retentionMode: RetentionMode = .keepForever
  private(set) var retentionDays = 30
  /// How many recordings the last switch to Forever kept; nil until then.
  private(set) var keptForever: Int?
  private(set) var error: String?
  private let environment: AppEnvironment
  /// Lists the input devices; the live one asks Core Audio.
  var listInputs: @Sendable () throws -> [AudioDeviceInfo] = { try AudioDevices.inputs() }

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  /// The rule the picker and the stepper currently describe.
  var retention: AudioRetention {
    switch retentionMode {
    case .keepForever: .keepForever
    case .keepDays: .keepDays(retentionDays)
    case .deleteAfterProcessing: .deleteAfterProcessing
    }
  }

  /// The sentence under the picker for the current selection.
  var footnote: String { retention.footnote }

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
    await measureFolderUsage()
  }

  func refreshDevices() {
    do {
      devices = try listInputs()
    } catch {
      devices = []
      self.error = "Input devices could not be listed: \(error)"
    }
  }

  /// `AudioFolderUsage.measure` off the main actor. A folder that does not
  /// exist yet holds no recordings and reads as zero; one that cannot be
  /// read is "unavailable".
  func measureFolderUsage() async {
    folderUsage = .measuring
    let folder = audioFolder
    let measured = await Task.detached(priority: .utility) { () -> Int64? in
      guard FileManager.default.fileExists(atPath: folder.path) else { return 0 }
      return try? AudioFolderUsage.measure(folder)
    }.value
    guard folder == audioFolder else { return }
    folderUsage = measured.map(FolderUsage.bytes) ?? .unavailable
  }

  func setInputDevice(_ uid: String?) async {
    inputDeviceUID = uid
    await save { $0.inputDeviceUID = uid }
  }

  func setAudioFolder(_ url: URL) async {
    audioFolder = url
    await save { $0.audioFolder = url }
    await measureFolderUsage()
  }

  /// Saves the rule; Forever also keeps every recording still on disk and
  /// reports how many in `keptForever`. Nothing is ever deleted here.
  func setRetention(mode: RetentionMode, days: Int) async {
    retentionMode = mode
    retentionDays = min(max(Self.dayRange.lowerBound, days), Self.dayRange.upperBound)
    let retention = self.retention
    await save { $0.defaultRetention = retention }
    guard mode == .keepForever, error == nil else { return }
    do {
      keptForever = try await environment.sweep.keepAll()
    } catch {
      self.error = "Recordings could not be marked as kept: \(error)"
    }
  }

  private func save(_ mutate: (inout Settings) -> Void) async {
    do {
      try await environment.updateSettings(mutate)
      error = nil
    } catch {
      self.error = "Setting could not be saved: \(error)"
    }
  }
}
