import Foundation

// The only protocols the app target defines. Each one stands in front of a
// system framework that has no seam of its own (ServiceManagement, TCC,
// EventKit, Sparkle); package types are injected as they are.

/// `SMAppService.Status`, spelled without ServiceManagement.
enum LoginItemStatus: Sendable, Equatable {
  case notRegistered
  case enabled
  case requiresApproval
  case notFound
}

@MainActor
protocol LoginItemControlling: AnyObject {
  var status: LoginItemStatus { get }
  func setEnabled(_ enabled: Bool) throws
  func openSystemSettings()
}

enum PermissionState: Sendable, Equatable {
  /// Never asked, or the framework has no status API (system audio, local
  /// network) and nothing has probed it yet.
  case unknown
  case granted
  case denied
}

enum PermissionKind: String, Sendable, Equatable, CaseIterable, Identifiable {
  case microphone
  case systemAudio
  case calendar
  case localNetwork

  var id: String { rawValue }

  /// Recording needs the microphone and the system audio tap; calendar and
  /// local network improve titles and enable the phone, and can wait.
  var isRequired: Bool {
    switch self {
    case .microphone, .systemAudio: true
    case .calendar, .localNetwork: false
    }
  }
}

@MainActor
protocol PermissionsChecking: AnyObject {
  func state(of kind: PermissionKind) async -> PermissionState
  /// Runs the system prompt (or the probe) and returns the resulting state.
  func request(_ kind: PermissionKind) async -> PermissionState
  /// Opens the System Settings pane where the user can change the answer.
  func openSystemSettings(for kind: PermissionKind)
}

struct CalendarAttendee: Sendable, Equatable, Hashable {
  var name: String
  var email: String?
  /// The calendar's owner; the pipeline adds "me" itself.
  var isCurrentUser: Bool
}

struct CalendarEvent: Sendable, Equatable, Hashable, Identifiable {
  var id: String
  var title: String
  var start: Date
  var end: Date
  var attendees: [CalendarAttendee]
}

@MainActor
protocol CalendarProviding: AnyObject {
  /// Every event of the calendar day containing `day`; empty without access.
  func events(on day: Date) async throws -> [CalendarEvent]
}

@MainActor
protocol UpdaterControlling: AnyObject {
  var canCheckForUpdates: Bool { get }
  var automaticallyChecksForUpdates: Bool { get set }
  var lastUpdateCheckDate: Date? { get }
  func checkForUpdates()
}
