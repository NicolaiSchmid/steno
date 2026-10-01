import AppKit
import StenoBridge
import SwiftUI

/// The onboarding window: the web UI at `#/onboarding` over
/// `OnboardingBridge` (plan Decision 1). Fixed at 560 by 620: tall enough for
/// page 1's four rows, short enough to sit above the Dock on a 768-point
/// display (the smoke runner's) with its footer reachable; page 2's forms
/// scroll under the pinned footer. No title bar:
/// the page's H1 is the window's one title and the page paints up to the top
/// edge, leaving the traffic lights their inset. The bridge is created once
/// with the window's content and driven by `.task` for the window's
/// lifetime; it owns the onboarding view model. The view hands it only what
/// a host cannot reach on its own: the scene actions behind `window.open`
/// and `window.close`.
struct OnboardingWindow: View {
  static let size = CGSize(width: 560, height: 620)

  let controller: AppController
  @State private var bridge: OnboardingBridge
  @Environment(\.openWindow) private var openWindow
  @Environment(\.dismissWindow) private var dismissWindow

  init(controller: AppController) {
    self.controller = controller
    _bridge = State(initialValue: OnboardingBridge(controller: controller))
  }

  var body: some View {
    // The page under the hidden title bar: `ignoresSafeArea` inside the
    // frame lets the web view cover the title bar's region as well, so no
    // bare strip shows above it and the traffic lights float over the page's
    // own inset. The frame fixes the page's size; the window adds the title
    // bar's height to it.
    WebWindowView(route: "#/onboarding", host: bridge)
      .ignoresSafeArea()
      .frame(width: Self.size.width, height: Self.size.height)
      .task {
        bridge.openWindow = { [controller, openWindow] request in
          openRequestedWindow(request, controller: controller, openWindow: openWindow)
        }
        bridge.closeWindow = { [dismissWindow] in dismissWindow(id: "onboarding") }
        await bridge.run()
      }
      // The window's own close button counts as having seen the pages, as
      // before: the opener then returns only for a missing required permission.
      .onDisappear { bridge.model.markCompleted() }
      // Finish, or both setup rows handled: the host closes its own window,
      // as the SwiftUI window did on the same change.
      .onChange(of: bridge.model.finished) { _, finished in
        if finished { dismissWindow(id: "onboarding") }
      }
  }
}
