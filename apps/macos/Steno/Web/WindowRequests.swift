import AppKit
import StenoBridge
import SwiftUI

/// `window.open` from a page: the main window, with a meeting to show when
/// the request names one; Settings, on a section when it names one; or
/// onboarding. Each web window installs this on its bridge with the scene's
/// own `openWindow`, so both windows open the others the same way.
@MainActor
func openRequestedWindow(
  _ request: WindowParams, controller: AppController, openWindow: OpenWindowAction
) {
  switch request.window {
  case .main:
    if let meetingID = request.meetingID { controller.requestedMeetingID = meetingID }
    openWindow(id: "main")
  case .settings:
    if let section = request.section.flatMap({ SettingsSection(rawValue: $0.rawValue) }) {
      controller.openSettings(section)
    }
    openWindow(id: "settings")
  case .onboarding:
    openWindow(id: "onboarding")
  }
  NSApp.activate()
}
