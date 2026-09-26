import Foundation
import StenoAdapters
import StenoCore

/// Obsidian: `Settings.obsidian` (nil means not configured), validated on
/// save with `ObsidianFolderDestination.validate()`; `ObsidianError`
/// messages are shown verbatim.
@MainActor
@Observable
final class ObsidianSettingsViewModel {
  var enabled = false
  var vaultPath = ""
  var peopleFolder = ""
  var includeAudio = false
  var taskTag = ""
  private(set) var error: String?
  private(set) var validationMessage: String?
  private(set) var saved = false
  private let environment: AppEnvironment

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  func load() async {
    do {
      let settings = try await environment.settings.load()
      if let obsidian = settings.obsidian {
        enabled = true
        vaultPath = obsidian.vaultPath
        peopleFolder = obsidian.peopleFolder ?? ""
        includeAudio = obsidian.includeAudio
        taskTag = obsidian.taskTag ?? ""
      } else {
        enabled = false
      }
    } catch {
      self.error = "Settings could not be loaded: \(error)"
    }
  }

  /// The typed settings as entered; nil when disabled.
  var draft: ObsidianSettings? {
    guard enabled else { return nil }
    let people = peopleFolder.trimmingCharacters(in: .whitespaces)
    let tag = taskTag.trimmingCharacters(in: .whitespaces)
    return ObsidianSettings(
      vaultPath: vaultPath, peopleFolder: people.isEmpty ? nil : people,
      includeAudio: includeAudio, taskTag: tag.isEmpty ? nil : tag)
  }

  /// Validates through the destination and stores the settings; a failed
  /// validation stores nothing and reports the destination's message.
  func save() async {
    saved = false
    validationMessage = nil
    if let draft {
      do {
        try await ObsidianFolderDestination(settings: draft).validate()
      } catch let obsidian as ObsidianError {
        validationMessage = obsidian.description
        return
      } catch {
        validationMessage = String(describing: error)
        return
      }
    }
    do {
      try await environment.updateSettings { $0.obsidian = draft }
      saved = true
      error = nil
    } catch {
      self.error = "Settings could not be saved: \(error)"
    }
  }
}
