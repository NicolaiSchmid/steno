import Foundation
import StenoCore

/// General: launch at login, meeting detection, default template.
@MainActor
@Observable
final class GeneralSettingsViewModel {
  private(set) var loginItem: LoginItemStatus
  private(set) var detectionEnabled = true
  private(set) var defaultTemplateID = SummaryTemplate.defaultID
  private(set) var error: String?
  let templates = SummaryTemplate.bundled
  private let environment: AppEnvironment

  init(environment: AppEnvironment) {
    self.environment = environment
    self.loginItem = environment.loginItem.status
  }

  func load() async {
    do {
      let settings = try await environment.settings.load()
      detectionEnabled = settings.meetingDetectionEnabled
      defaultTemplateID = settings.defaultTemplateID
      loginItem = environment.loginItem.status
    } catch {
      self.error = "Settings could not be loaded: \(error)"
    }
  }

  var launchAtLogin: Bool {
    loginItem == .enabled || loginItem == .requiresApproval
  }

  func setLaunchAtLogin(_ enabled: Bool) async {
    do {
      try environment.loginItem.setEnabled(enabled)
      try await update { $0.launchAtLogin = enabled }
    } catch {
      self.error = "Login item could not be changed: \(error)"
    }
    loginItem = environment.loginItem.status
  }

  func openLoginItemSettings() {
    environment.loginItem.openSystemSettings()
  }

  func setDetectionEnabled(_ enabled: Bool) async {
    detectionEnabled = enabled
    do {
      try await update { $0.meetingDetectionEnabled = enabled }
    } catch {
      self.error = "Setting could not be saved: \(error)"
    }
  }

  func setDefaultTemplate(_ id: String) async {
    guard SummaryTemplate.bundled(id: id) != nil else { return }
    defaultTemplateID = id
    do {
      try await update { $0.defaultTemplateID = id }
    } catch {
      self.error = "Setting could not be saved: \(error)"
    }
  }

  private func update(_ mutate: (inout Settings) -> Void) async throws {
    var settings = try await environment.settings.load()
    mutate(&settings)
    try await environment.settings.save(settings)
  }
}
