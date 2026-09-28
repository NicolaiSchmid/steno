import Foundation
import StenoCore

/// The menu bar item's presentation: the processing queue from
/// `observeMeetings()`, recent meetings and the login item. Each queue
/// row's stage, bar and estimate come from `AppController.progress`, which
/// the view reads beside this model; recording itself is
/// `RecordingController`, read the same way. `observe()` runs for the app's
/// lifetime from `AppController.launch()` and ends when it is cancelled.
@MainActor
@Observable
final class MenuBarViewModel {
  /// A meeting in `.queued` or `.processing`; its progress is
  /// `ProcessingProgressModel.entry(for:)`.
  struct QueueItem: Identifiable, Equatable, Sendable {
    var meeting: Meeting
    var id: UUID { meeting.id }
  }

  private(set) var queue: [QueueItem] = []
  private(set) var recent: [Meeting] = []
  private(set) var launchAtLogin: LoginItemStatus
  private(set) var lastError: String?

  private let environment: AppEnvironment
  private var meetings: [Meeting] = []

  init(environment: AppEnvironment) {
    self.environment = environment
    self.launchAtLogin = environment.loginItem.status
  }

  /// Follows the meeting list until cancelled.
  func observe() async {
    do {
      for try await meetings in environment.store.observeMeetings() {
        self.meetings = meetings
        rebuildQueue()
      }
    } catch {
      lastError = "Meeting list unavailable: \(error)"
    }
  }

  // MARK: - Queue

  private func rebuildQueue() {
    queue =
      meetings
      .filter { $0.state == .queued || $0.state == .processing }
      .map { QueueItem(meeting: $0) }
      .sorted { $0.meeting.startedAt < $1.meeting.startedAt }
    recent = Array(
      meetings
        .filter { $0.state == .ready || $0.state.isFailed }
        .sorted { $0.startedAt > $1.startedAt }
        .prefix(5))
  }

  // MARK: - Login item

  func setLaunchAtLogin(_ enabled: Bool) async {
    do {
      try await environment.setLaunchAtLogin(enabled)
    } catch {
      lastError = "Login item could not be changed: \(error)"
    }
    launchAtLogin = environment.loginItem.status
  }

  func refreshLoginItem() {
    launchAtLogin = environment.loginItem.status
  }

  func openLoginItemSettings() {
    environment.loginItem.openSystemSettings()
  }

  func checkForUpdates() {
    environment.updater.checkForUpdates()
  }
}
