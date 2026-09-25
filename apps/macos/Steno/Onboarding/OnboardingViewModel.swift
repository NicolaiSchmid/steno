import Foundation

/// Permission onboarding: microphone, system audio (both required), then
/// calendar (optional) and local network (optional; the prompt comes with
/// the first phone pairing, so this step only explains). Done when every
/// required step is granted.
@MainActor
@Observable
final class OnboardingViewModel {
  struct Step: Identifiable, Equatable, Sendable {
    var kind: PermissionKind
    var state: PermissionState
    var id: PermissionKind { kind }
    var isRequired: Bool { kind.isRequired }
  }

  private(set) var steps = PermissionKind.allCases.map { Step(kind: $0, state: .unknown) }
  private(set) var requesting: PermissionKind?
  private(set) var skipped: Set<PermissionKind> = []
  private let permissions: any PermissionsChecking

  init(permissions: any PermissionsChecking) {
    self.permissions = permissions
  }

  func load() async {
    for index in steps.indices {
      steps[index].state = await permissions.state(of: steps[index].kind)
    }
  }

  /// The first step that is neither granted nor skipped; the last one when
  /// every step is handled.
  var current: PermissionKind {
    steps.first { $0.state != .granted && !skipped.contains($0.kind) }?.kind ?? .localNetwork
  }

  var isComplete: Bool {
    steps.filter(\.isRequired).allSatisfy { $0.state == .granted }
  }

  /// Every step handled: required ones granted, optional ones granted or
  /// skipped.
  var isFinished: Bool {
    isComplete && steps.filter { !$0.isRequired }.allSatisfy {
      $0.state == .granted || skipped.contains($0.kind)
    }
  }

  func state(of kind: PermissionKind) -> PermissionState {
    steps.first { $0.kind == kind }?.state ?? .unknown
  }

  func request(_ kind: PermissionKind) async {
    requesting = kind
    let state = await permissions.request(kind)
    if let index = steps.firstIndex(where: { $0.kind == kind }) {
      steps[index].state = state
    }
    requesting = nil
  }

  func openSystemSettings(_ kind: PermissionKind) {
    permissions.openSystemSettings(for: kind)
  }

  /// Optional steps can be skipped; required ones cannot.
  func skip(_ kind: PermissionKind) {
    guard !kind.isRequired else { return }
    skipped.insert(kind)
  }
}

extension PermissionKind {
  var title: String {
    switch self {
    case .microphone: "Microphone"
    case .systemAudio: "System audio"
    case .calendar: "Calendar"
    case .localNetwork: "Local network"
    }
  }

  var explanation: String {
    switch self {
    case .microphone:
      "Steno records your side of a meeting from the microphone. Required."
    case .systemAudio:
      "Steno records the other side from the apps playing audio on this Mac. macOS asks once, during a short test recording. Required."
    case .calendar:
      "Steno names recordings after the calendar event they overlap and suggests the attendees as speakers. Optional."
    case .localNetwork:
      "The Steno iPhone app sends recordings over your Wi-Fi. macOS asks when you pair the first phone. Optional."
    }
  }
}
