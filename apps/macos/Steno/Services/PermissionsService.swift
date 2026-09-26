import AVFoundation
import AppKit
import EventKit
import StenoAudio

/// TCC as far as the frameworks expose it. Microphone and calendar have
/// status APIs; the system audio tap has none, so the state is what the last
/// `SystemAudioPermission.request()` probe found (remembered in
/// `UserDefaults`); the local network prompt appears on the first Bonjour
/// registration and has no status API either, so it stays `.unknown` until
/// the listener reports a policy denial.
@MainActor
final class PermissionsService: PermissionsChecking {
  static let systemAudioGrantedKey = "steno.systemAudioGranted"
  private let defaults: UserDefaults
  private let eventStore: EKEventStore

  init(defaults: UserDefaults = .standard, eventStore: EKEventStore) {
    self.defaults = defaults
    self.eventStore = eventStore
  }

  func state(of kind: PermissionKind) async -> PermissionState {
    switch kind {
    case .microphone:
      switch AVCaptureDevice.authorizationStatus(for: .audio) {
      case .authorized: return .granted
      case .denied, .restricted: return .denied
      case .notDetermined: return .unknown
      @unknown default: return .unknown
      }
    case .calendar:
      switch EKEventStore.authorizationStatus(for: .event) {
      case .fullAccess: return .granted
      case .denied, .restricted, .writeOnly: return .denied
      case .notDetermined: return .unknown
      @unknown default: return .unknown
      }
    case .systemAudio:
      return defaults.bool(forKey: Self.systemAudioGrantedKey) ? .granted : .unknown
    case .localNetwork:
      return .unknown
    }
  }

  func request(_ kind: PermissionKind) async -> PermissionState {
    switch kind {
    case .microphone:
      let granted = await SystemAudioPermission.microphone()
      return granted ? .granted : .denied
    case .calendar:
      // The completion-handler form: `EKEventStore` is not Sendable, so the
      // async variant cannot be awaited from the main actor under strict
      // concurrency.
      let granted = await withCheckedContinuation { (continuation: CheckedContinuation<Bool, Never>) in
        eventStore.requestFullAccessToEvents { granted, _ in
          continuation.resume(returning: granted)
        }
      }
      return granted ? .granted : .denied
    case .systemAudio:
      // Runs the real tap pipeline while `afplay` plays a tone; the first
      // run surfaces the TCC prompt. Only the deadline means denied.
      let granted = await SystemAudioPermission.request()
      defaults.set(granted, forKey: Self.systemAudioGrantedKey)
      return granted ? .granted : .denied
    case .localNetwork:
      // The prompt appears when the handover listener first advertises;
      // nothing to request here.
      return .unknown
    }
  }

  func openSystemSettings(for kind: PermissionKind) {
    let pane =
      switch kind {
      case .microphone: "Privacy_Microphone"
      case .systemAudio: "Privacy_AudioCapture"
      case .calendar: "Privacy_Calendars"
      case .localNetwork: "Privacy_LocalNetwork"
      }
    if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)") {
      NSWorkspace.shared.open(url)
    }
  }
}
