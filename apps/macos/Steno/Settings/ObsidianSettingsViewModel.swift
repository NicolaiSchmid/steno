import Foundation
import StenoAdapters
import StenoCore

/// Export: `Settings.obsidian` (nil means off), validated on save with
/// `ObsidianFolderDestination.validate()`; `ObsidianError` messages are
/// shown verbatim. `commit()` saves when the form differs from what is
/// stored; the toggle and the folder chooser call it, text fields on blur.
@MainActor
@Observable
final class ObsidianSettingsViewModel: SettingsSectionModel {
  var enabled = false
  var vaultPath = ""
  var peopleFolder = ""
  var includeAudio = false
  var taskTag = ""
  var error: String?
  var errorDetails: String?
  private(set) var validationMessage: String?
  /// True after a save; the view clears it by saving again or leaving.
  var saved = false
  private var stored: ObsidianSettings?
  private let environment: AppEnvironment

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  func load() async {
    do {
      let settings = try await environment.settings.load()
      stored = settings.obsidian
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
      fail("Settings could not be loaded.", error)
    }
  }

  /// The vault's folder name, for the folder row; empty until chosen.
  var vaultName: String { vaultURL?.lastPathComponent ?? "" }

  var vaultURL: URL? {
    vaultPath.isEmpty ? nil : URL(fileURLWithPath: vaultPath, isDirectory: true)
  }

  /// On, but no folder chosen yet: the status row asks for one instead of
  /// reporting a validation failure.
  var needsVault: Bool { enabled && vaultPath.trimmingCharacters(in: .whitespaces).isEmpty }

  /// The typed settings as entered; nil when disabled.
  var draft: ObsidianSettings? {
    guard enabled else { return nil }
    let people = peopleFolder.trimmingCharacters(in: .whitespaces)
    let tag = taskTag.trimmingCharacters(in: .whitespaces)
    return ObsidianSettings(
      vaultPath: vaultPath, peopleFolder: people.isEmpty ? nil : people,
      includeAudio: includeAudio, taskTag: tag.isEmpty ? nil : tag)
  }

  // MARK: Actions

  func setEnabled(_ on: Bool) async {
    enabled = on
    await commit()
  }

  func chooseVault(_ url: URL) async {
    vaultPath = url.path
    await commit()
  }

  func setIncludeAudio(_ on: Bool) async {
    includeAudio = on
    await commit()
  }

  /// Saves when the draft differs from what is stored. On without a vault
  /// waits for the chooser and saves nothing.
  func commit() async {
    if needsVault {
      validationMessage = nil
      return
    }
    guard draft != stored else {
      validationMessage = nil
      return
    }
    await save()
  }

  /// Validates through the destination and stores the settings; a failed
  /// validation stores nothing and reports the destination's message.
  func save() async {
    saved = false
    validationMessage = nil
    let draft = self.draft
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
      stored = draft
      saved = true
      clearError()
    } catch {
      fail("Settings could not be saved.", error)
    }
  }
}
