import Foundation

// Fakes for the four app protocols only. `AppEnvironment.preview()` (also
// the `-steno-ui-testing` launch mode) and the unit tests use them; module
// fakes come from the modules' own `Testing/` folders.

@MainActor
final class FakeLoginItem: LoginItemControlling {
  var status: LoginItemStatus
  private(set) var changes: [Bool] = []

  init(status: LoginItemStatus = .notRegistered) {
    self.status = status
  }

  func setEnabled(_ enabled: Bool) throws {
    changes.append(enabled)
    status = enabled ? .enabled : .notRegistered
  }

  func openSystemSettings() {}
}

@MainActor
final class FakePermissions: PermissionsChecking {
  var states: [PermissionKind: PermissionState]
  /// What `request` answers per kind; defaults to `.granted`.
  var answers: [PermissionKind: PermissionState] = [:]
  private(set) var requests: [PermissionKind] = []
  private(set) var openedPanes: [PermissionKind] = []

  init(states: [PermissionKind: PermissionState] = [:]) {
    self.states = states
  }

  static func allGranted() -> FakePermissions {
    FakePermissions(
      states: Dictionary(uniqueKeysWithValues: PermissionKind.allCases.map { ($0, .granted) }))
  }

  func state(of kind: PermissionKind) async -> PermissionState {
    states[kind] ?? .unknown
  }

  func request(_ kind: PermissionKind) async -> PermissionState {
    requests.append(kind)
    let answer = answers[kind] ?? .granted
    states[kind] = answer
    return answer
  }

  func openSystemSettings(for kind: PermissionKind) {
    openedPanes.append(kind)
  }
}

@MainActor
final class FakeCalendar: CalendarProviding {
  var events: [CalendarEvent]

  init(events: [CalendarEvent] = []) {
    self.events = events
  }

  func events(on day: Date) async throws -> [CalendarEvent] { events }
}

@MainActor
final class FakeUpdater: UpdaterControlling {
  var canCheckForUpdates = true
  var automaticallyChecksForUpdates = true
  var lastUpdateCheckDate: Date?

  init() {}

  func checkForUpdates() {}
}
