import AppKit
import StenoBridge
import SwiftUI

/// The Settings window: the web UI at `#/settings` over `SettingsBridge`
/// (plan Decision 1). Fixed at 760 by 520, no title bar: the page paints up
/// to the top edge and leaves the traffic lights their inset. The bridge is
/// created once with the window's content and driven by `.task` for the
/// window's lifetime; it owns the six section view models. The view hands it
/// only what a host cannot reach on its own: the window's appearance for the
/// `app` snapshot and the scene action behind `window.open`. Deep links
/// arrive through `AppController.requestedSettingsSection`, which the `app`
/// snapshot carries and the page selects.
struct SettingsWindow: View {
  static let size = CGSize(width: 760, height: 520)

  let controller: AppController
  @State private var bridge: SettingsBridge
  @Environment(\.openWindow) private var openWindow

  init(controller: AppController) {
    self.controller = controller
    _bridge = State(initialValue: SettingsBridge(controller: controller))
  }

  var body: some View {
    // The page under the hidden title bar: `ignoresSafeArea` inside the
    // frame lets the web view cover the title bar's region as well, so no
    // bare strip shows above it and the traffic lights float over the page's
    // own inset. The frame fixes the page's size; the window adds the title
    // bar's height to it.
    WebWindowView(route: "#/settings", host: bridge)
      .ignoresSafeArea()
      .frame(width: Self.size.width, height: Self.size.height)
      .task {
        bridge.openWindow = windowOpener(controller: controller, openWindow: openWindow)
        await bridge.run()
      }
  }
}
