import Foundation

/// What a recording control renders for a recorder state and the denied
/// required permissions: label, role, whether it is enabled or busy, whether
/// the in-person mode is offered, and the reason it is disabled. The sidebar
/// control, the Record menu and the menu bar item all render from this, so
/// the state table lives in one place and one test.
struct RecordingControlPresentation: Equatable, Sendable {
  enum Role {
    /// Starts a recording.
    case primary
    /// Stops the recording.
    case stop
  }

  var label: String
  var role: Role
  var isEnabled: Bool
  /// A transient state: a spinner stands in for the label.
  var isBusy: Bool
  /// "Record in person" is offered only when idle with nothing denied.
  var offersInPerson: Bool
  /// Shown under the control when it is disabled for a lasting reason.
  var disabledReason: String?

  /// Only required kinds in `denied` count; an optional kind never disables.
  static func make(state: RecordingState, denied: [PermissionKind]) -> RecordingControlPresentation
  {
    let denied = denied.filter(\.isRequired)
    switch state {
    case .idle:
      return RecordingControlPresentation(
        label: "Record call", role: .primary, isEnabled: denied.isEmpty, isBusy: false,
        offersInPerson: denied.isEmpty,
        disabledReason: denied.isEmpty ? nil : denied.map(\.deniedMessage).joined(separator: " "))
    case .starting:
      return RecordingControlPresentation(
        label: "Starting…", role: .primary, isEnabled: false, isBusy: true, offersInPerson: false,
        disabledReason: nil)
    case .recording:
      return RecordingControlPresentation(
        label: "Stop", role: .stop, isEnabled: true, isBusy: false, offersInPerson: false,
        disabledReason: nil)
    case .stopping:
      return RecordingControlPresentation(
        label: "Finishing…", role: .stop, isEnabled: false, isBusy: true, offersInPerson: false,
        disabledReason: nil)
    }
  }
}

extension PermissionKind {
  /// The sentence a control shows when this permission is denied.
  var deniedMessage: String {
    switch self {
    case .microphone: "Microphone access is denied."
    case .systemAudio: "System audio access is denied."
    case .calendar: "Calendar access is denied."
    case .localNetwork: "Local network access is denied."
    }
  }
}
