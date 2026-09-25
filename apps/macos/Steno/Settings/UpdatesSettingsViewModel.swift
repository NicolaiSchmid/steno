import Foundation

/// Updates: Sparkle's automatic check bound through `UpdaterControlling`,
/// Check now, the last check, and the running version.
@MainActor
@Observable
final class UpdatesSettingsViewModel {
  private let updater: any UpdaterControlling
  let version: String
  let build: String

  init(updater: any UpdaterControlling, bundle: Bundle = .main) {
    self.updater = updater
    self.version = bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.0.0"
    self.build = bundle.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "0"
  }

  var automaticallyChecks: Bool {
    get { updater.automaticallyChecksForUpdates }
    set { updater.automaticallyChecksForUpdates = newValue }
  }

  var canCheck: Bool { updater.canCheckForUpdates }
  var lastCheck: Date? { updater.lastUpdateCheckDate }

  func checkNow() {
    updater.checkForUpdates()
  }
}
