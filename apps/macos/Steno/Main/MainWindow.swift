import AppKit
import StenoBridge
import SwiftUI

/// The main window: the web UI at `#/main` over `MainWindowBridge` (plan
/// Decision 1). The bridge is created once with the window's content and
/// driven by `.task` for the window's lifetime; it owns the list and detail
/// models and every rule of the window. The view hands it only what a
/// host cannot reach on its own: the window's appearance for the `app`
/// snapshot and the scene actions behind `window.open`. No title bar and no
/// toolbar: the page paints up to the top edge and leaves the traffic
/// lights their inset.
struct MainWindow: View {
  let controller: AppController
  @State private var bridge: MainWindowBridge
  @Environment(\.openWindow) private var openWindow

  /// 960 x 600, the size the UI smoke test reviews the window at.
  static let minimumSize = CGSize(width: 960, height: 600)

  init(controller: AppController) {
    self.controller = controller
    _bridge = State(initialValue: MainWindowBridge(controller: controller))
  }

  var body: some View {
    // `ignoresSafeArea` inside the frame: the page covers the hidden title
    // bar's region too, so the traffic lights float over its own inset and no
    // bare strip shows above it.
    WebWindowView(route: "#/main", host: bridge)
      .ignoresSafeArea()
      .frame(minWidth: Self.minimumSize.width, minHeight: Self.minimumSize.height)
      .task {
        bridge.openWindow = { [controller, openWindow] request in
          openRequestedWindow(request, controller: controller, openWindow: openWindow)
        }
        await bridge.run()
      }
  }
}

/// The "focus the meeting search" action `AppCommands` runs for ⌘F. Nothing
/// publishes it yet: the field now lives in the page and the contract has
/// no `ui.focusSearch` event for the host to send, so the menu item stays
/// disabled until that topic lands (plan Decision 6 names it).
struct SearchFocusAction {
  let run: @MainActor () -> Void
}

private struct SearchFocusKey: FocusedValueKey {
  typealias Value = SearchFocusAction
}

extension FocusedValues {
  var searchFocus: SearchFocusAction? {
    get { self[SearchFocusKey.self] }
    set { self[SearchFocusKey.self] = newValue }
  }
}
