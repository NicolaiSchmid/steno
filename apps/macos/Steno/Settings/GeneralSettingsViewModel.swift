import Foundation
import StenoCore

/// General: launch at login, meeting detection, the calendar permission that
/// names meetings, the default template, and the update status.
@MainActor
@Observable
final class GeneralSettingsViewModel {
  private(set) var loginItem: LoginItemStatus
  private(set) var detectionEnabled = true
  private(set) var defaultTemplateID = SummaryTemplate.defaultID
  private(set) var calendarPermission: PermissionState = .unknown
  private(set) var requestingCalendar = false
  private(set) var error: String?
  private(set) var errorDetails: String?
  let templates = SummaryTemplate.bundled
  let version = AppVersion.marketing
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
      fail("Settings could not be loaded.", error)
    }
    calendarPermission = await environment.permissions.state(of: .calendar)
  }

  var launchAtLogin: Bool { loginItem.isOn }

  var selectedTemplate: SummaryTemplate? {
    templates.first { $0.id == defaultTemplateID }
  }

  func setLaunchAtLogin(_ enabled: Bool) async {
    do {
      try await environment.setLaunchAtLogin(enabled)
    } catch {
      fail("Opening Steno at login could not be changed.", error)
    }
    loginItem = environment.loginItem.status
  }

  func openLoginItemSettings() {
    environment.loginItem.openSystemSettings()
  }

  func setDetectionEnabled(_ enabled: Bool) async {
    detectionEnabled = enabled
    await save { $0.meetingDetectionEnabled = enabled }
  }

  func setDefaultTemplate(_ id: String) async {
    guard SummaryTemplate.bundled(id: id) != nil else { return }
    defaultTemplateID = id
    await save { $0.defaultTemplateID = id }
  }

  // MARK: Calendar

  func requestCalendar() async {
    requestingCalendar = true
    defer { requestingCalendar = false }
    calendarPermission = await environment.permissions.request(.calendar)
  }

  func openCalendarSettings() {
    environment.permissions.openSystemSettings(for: .calendar)
  }

  // MARK: Updates

  var updater: any UpdaterControlling { environment.updater }

  var automaticallyChecksForUpdates: Bool {
    get { environment.updater.automaticallyChecksForUpdates }
    set { environment.updater.automaticallyChecksForUpdates = newValue }
  }

  var automaticallyDownloadsUpdates: Bool {
    get { environment.updater.automaticallyDownloadsUpdates }
    set { environment.updater.automaticallyDownloadsUpdates = newValue }
  }

  func checkForUpdates() {
    environment.updater.checkForUpdates()
  }

  /// "Up to date, checked 2 hours ago", "Update available: 0.9.1", "Could not
  /// check for updates" or "Not checked yet".
  func updateStatusText(now: Date) -> String {
    Self.updateStatus(
      outcome: environment.updater.lastOutcome, lastCheck: environment.updater.lastUpdateCheckDate,
      now: now)
  }

  /// The failure text of the last check, for the details disclosure.
  var updateFailureDetails: String? {
    if case .failed(let message) = environment.updater.lastOutcome { return message }
    return nil
  }

  nonisolated static func updateStatus(
    outcome: UpdateCheckOutcome, lastCheck: Date?, now: Date
  ) -> String {
    switch outcome {
    case .notChecked:
      return "Not checked yet"
    case .upToDate:
      guard let lastCheck else { return "Up to date" }
      let formatter = RelativeDateTimeFormatter()
      formatter.unitsStyle = .full
      return "Up to date, checked \(formatter.localizedString(for: lastCheck, relativeTo: now))"
    case .available(let version):
      return "Update available: \(version)"
    case .failed:
      return "Could not check for updates"
    }
  }

  // MARK: Errors

  private func save(_ mutate: (inout Settings) -> Void) async {
    do {
      try await environment.updateSettings(mutate)
      error = nil
      errorDetails = nil
    } catch {
      fail("The setting could not be saved.", error)
    }
  }

  private func fail(_ message: String, _ error: any Error) {
    self.error = message
    errorDetails = String(describing: error)
  }
}
