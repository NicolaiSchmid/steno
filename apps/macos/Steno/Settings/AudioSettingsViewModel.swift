import AppKit
import Foundation
import StenoAudio
import StenoCore

/// Recording: the two recording permissions, the input device, the
/// recordings folder with its size, and the default retention.
@MainActor
@Observable
final class AudioSettingsViewModel: SettingsSectionModel {
  /// Picker order: the safe choice first, the destructive one last.
  enum RetentionMode: String, CaseIterable, Identifiable, Sendable {
    case keepForever
    case keepDays
    case deleteAfterProcessing

    var id: String { rawValue }

    /// The option's label; the day count is spelled out for `keepDays`.
    func title(days: Int) -> String {
      switch self {
      case .keepForever: "Forever"
      case .keepDays: "For \(days) days"
      case .deleteAfterProcessing: "Until processed, then delete"
      }
    }

    /// One sentence under the picker, as the retention plan words it.
    func footnote(days: Int) -> String {
      switch self {
      case .keepForever:
        "Recordings stay in the folder above until you delete a meeting."
      case .keepDays:
        "Each recording is deleted \(days) days after it was processed and exported. Transcripts, summaries and exports are never deleted by this rule."
      case .deleteAfterProcessing:
        "Each recording is deleted as soon as it was transcribed, summarised and exported. Transcripts, summaries and exports stay."
      }
    }
  }

  enum FolderUsage: Equatable, Sendable {
    case measuring
    case bytes(Int64)
    case unavailable

    var text: String {
      switch self {
      case .measuring: "Measuring…"
      case .bytes(let bytes):
        "Recordings use \(ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file))"
      case .unavailable: "Size unavailable"
      }
    }
  }

  static let recordingPermissions: [PermissionKind] = [.microphone, .systemAudio]

  private(set) var devices: [AudioDeviceInfo] = []
  private(set) var inputDeviceUID: String?
  private(set) var audioFolder: URL = Settings.defaultAudioFolder
  private(set) var folderUsage: FolderUsage = .measuring
  private(set) var retentionMode: RetentionMode = .keepDays
  private(set) var retentionDays = 30
  private(set) var permissions: [PermissionKind: PermissionState] = [:]
  private(set) var requesting: PermissionKind?
  var error: String?
  var errorDetails: String?
  private let environment: AppEnvironment
  /// Lists the input devices; the live one asks Core Audio.
  var listInputs: @Sendable () throws -> [AudioDeviceInfo] = { try AudioDevices.inputs() }
  /// Sums the folder; the live one walks the file system.
  var measureFolder: @Sendable (URL) throws -> Int64 = { try AudioSettingsViewModel.folderSize($0) }

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
      fail("Settings could not be loaded.", error)
    }
    refreshDevices()
    await refreshPermissions()
    await measureFolderUsage()
  }

  var folderName: String { audioFolder.lastPathComponent }

  var retentionFootnote: String { retentionMode.footnote(days: retentionDays) }

  // MARK: Devices

  func refreshDevices() {
    do {
      devices = try listInputs()
    } catch {
      devices = []
      fail("Microphones could not be listed.", error)
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

  // MARK: Folder

  func setAudioFolder(_ url: URL) async {
    audioFolder = url
    await save { $0.audioFolder = url }
    await measureFolderUsage()
  }

  func revealFolder() {
    NSWorkspace.shared.activateFileViewerSelecting([audioFolder])
  }

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

  /// Every regular file under `url`, summed; a missing folder is zero bytes.
  nonisolated static func folderSize(_ url: URL) throws -> Int64 {
    let manager = FileManager.default
    guard manager.fileExists(atPath: url.path) else { return 0 }
    guard
      let enumerator = manager.enumerator(
        at: url, includingPropertiesForKeys: [.fileSizeKey, .isRegularFileKey],
        options: [.skipsHiddenFiles])
    else { throw CocoaError(.fileReadUnknown) }
    var total: Int64 = 0
    for case let file as URL in enumerator {
      let values = try file.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
      guard values.isRegularFile == true else { continue }
      total += Int64(values.fileSize ?? 0)
    }
    return total
  }

  // MARK: Retention

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
