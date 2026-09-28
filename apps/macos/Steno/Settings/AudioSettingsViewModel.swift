import AppKit
import Foundation
import StenoAudio
import StenoCore

/// Recording: the two recording permissions, the input device, the
/// recordings folder with its disk usage, and the "Keep recordings" rule.
/// Switching the rule to Forever keeps every recording still on disk
/// (`RetentionSweep.keepAll()`); a shorter rule applies to new recordings
/// only.
@MainActor
@Observable
final class AudioSettingsViewModel: SettingsSectionModel {
  /// The picker's options, in display order: the safe choice first, the
  /// destructive one last.
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

    var text: String {
      switch self {
      case .measuring: "Measuring…"
      case .bytes(let bytes): "Recordings use \(ByteCountFormatter.fileSize(bytes))"
      case .unavailable: "Size unavailable"
      }
    }
  }

  static let dayRange = 1...3650
  static let recordingPermissions: [PermissionKind] = [.microphone, .systemAudio]

  private(set) var devices: [AudioDeviceInfo] = []
  private(set) var inputDeviceUID: String?
  private(set) var audioFolder: URL = Settings.defaultAudioFolder
  private(set) var folderUsage: FolderUsage = .measuring
  private(set) var retentionMode: RetentionMode = .keepForever
  private(set) var retentionDays = 30
  /// How many recordings the last switch to Forever kept; nil until then.
  private(set) var keptForever: Int?
  private(set) var permissions: [PermissionKind: PermissionState] = [:]
  private(set) var requesting: PermissionKind?
  var error: String?
  var errorDetails: String?
  private let environment: AppEnvironment
  /// Lists the input devices; the live one asks Core Audio.
  var listInputs: @Sendable () throws -> [AudioDeviceInfo] = { try AudioDevices.inputs() }
  /// Sums the folder; the live one is `AudioFolderUsage.measure`. A folder
  /// that does not exist yet holds no recordings and reads as zero; one
  /// that cannot be read throws and reads as "unavailable".
  var measureFolder: @Sendable (URL) throws -> Int64 = { folder in
    guard FileManager.default.fileExists(atPath: folder.path) else { return 0 }
    return try AudioFolderUsage.measure(folder)
  }

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

  var folderName: String { audioFolder.lastPathComponent }

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
      fail("Settings could not be loaded.", error)
    }
    refreshDevices()
    await refreshPermissions()
    await measureFolderUsage()
  }

  // MARK: Devices

  func refreshDevices() {
    do {
      devices = try listInputs()
    } catch {
      devices = []
      fail("Microphones could not be listed.", error)
    }
  }

  func setInputDevice(_ uid: String?) async {
    inputDeviceUID = uid
    await save { $0.inputDeviceUID = uid }
  }

  // MARK: Folder

  func setAudioFolder(_ url: URL) async {
    audioFolder = url
    await save { $0.audioFolder = url }
    await measureFolderUsage()
  }

  func revealFolder() {
    NSWorkspace.shared.activateFileViewerSelecting([audioFolder])
  }

  /// Measures off the main actor; a result for a folder that changed
  /// meanwhile is dropped.
  func measureFolderUsage() async {
    folderUsage = .measuring
    let folder = audioFolder
    let measure = measureFolder
    let result = await Task.detached(priority: .utility) { () -> FolderUsage in
      do {
        return .bytes(try measure(folder))
      } catch {
        return .unavailable
      }
    }.value
    guard folder == audioFolder else { return }
    folderUsage = result
  }

  // MARK: Retention

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
      fail("Recordings could not be marked as kept.", error)
    }
  }

  // MARK: Permissions

  func refreshPermissions() async {
    var states: [PermissionKind: PermissionState] = [:]
    for kind in Self.recordingPermissions {
      states[kind] = await environment.permissions.state(of: kind)
    }
    permissions = states
  }

  func state(of kind: PermissionKind) -> PermissionState {
    permissions[kind] ?? .unknown
  }

  var allPermissionsGranted: Bool {
    Self.recordingPermissions.allSatisfy { state(of: $0) == .granted }
  }

  func requestPermission(_ kind: PermissionKind) async {
    guard requesting == nil else { return }
    requesting = kind
    defer { requesting = nil }
    permissions[kind] = await environment.permissions.request(kind)
  }

  func openPermissionSettings(_ kind: PermissionKind) {
    environment.permissions.openSystemSettings(for: kind)
  }

  // MARK: Saving

  private func save(_ mutate: (inout Settings) -> Void) async {
    do {
      try await environment.updateSettings(mutate)
      clearError()
    } catch {
      fail("The setting could not be saved.", error)
    }
  }
}
