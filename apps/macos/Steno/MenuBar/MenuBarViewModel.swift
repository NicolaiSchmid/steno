import Foundation
import StenoCore

/// The menu bar item's presentation: the processing queue (pipeline
/// `progress` events joined with `observeMeetings()`), recent meetings and
/// the login item. Recording itself is `RecordingController`, which the
/// view reads through `AppController`. `observe()` runs for the app's
/// lifetime from `AppController.launch()` and ends when it is cancelled.
@MainActor
@Observable
final class MenuBarViewModel {
  struct QueueItem: Identifiable, Equatable, Sendable {
    var meeting: Meeting
    var stage: PipelineStage?
    var id: UUID { meeting.id }
    var fraction: Double { stage?.fraction ?? 0 }
  }

  private(set) var queue: [QueueItem] = []
  private(set) var recent: [Meeting] = []
  private(set) var launchAtLogin: LoginItemStatus
  private(set) var lastError: String?

  private let environment: AppEnvironment
  private var stages: [UUID: PipelineStage] = [:]
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

  /// Follows the pipeline's progress events until cancelled.
  func observeProgress() async {
    let stream = await environment.events.subscribe()
    for await event in stream {
      if case .progress(let meetingID, let stage) = event {
        stages[meetingID] = stage
        rebuildQueue()
      }
    }
  }

  // MARK: - Queue

  private func rebuildQueue() {
    let live = meetings.filter { $0.state == .queued || $0.state == .processing }
    queue =
      live
      .map { QueueItem(meeting: $0, stage: stages[$0.id]) }
      .sorted { $0.meeting.startedAt < $1.meeting.startedAt }
    for id in Array(stages.keys) where !live.contains(where: { $0.id == id }) {
      stages[id] = nil
    }
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
