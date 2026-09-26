import AppKit
import ServiceManagement

/// `SMAppService.mainApp`: the app registers itself as a login item. The
/// status `.requiresApproval` means the user must flip the switch in System
/// Settings > General > Login Items; `openSystemSettings` takes them there.
@MainActor
final class LoginItemController: LoginItemControlling {
  private let service = SMAppService.mainApp

  init() {}

  var status: LoginItemStatus {
    switch service.status {
    case .notRegistered: .notRegistered
    case .enabled: .enabled
    case .requiresApproval: .requiresApproval
    case .notFound: .notFound
    @unknown default: .notFound
    }
  }

  func setEnabled(_ enabled: Bool) throws {
    if enabled {
      try service.register()
    } else {
      try service.unregister()
    }
  }

  func openSystemSettings() {
    SMAppService.openSystemSettingsLoginItems()
  }
}
