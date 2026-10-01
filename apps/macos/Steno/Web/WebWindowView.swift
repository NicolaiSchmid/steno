import AppKit
import StenoBridge
import SwiftUI
import WebKit

/// One window's content: a `WKWebView` at a hash route of the bundled web UI
/// (plan Decision 1), talking to `host` through `WebBridge`. The web view has
/// no persistent storage, cannot open windows, paints no background of its
/// own, and its delegate cancels every navigation off the app's origin. The
/// window behind it is painted in the page's canvas colour (`WebCanvas`) for
/// the window's appearance, so while the page loads, and whenever the web
/// view is briefly empty, nothing but the page's own colour shows. In Debug,
/// `STENO_WEB_DEV_URL` swaps the bundle for the Vite dev server with hot
/// reload inside the real window; a release build never reads it.
struct WebWindowView: NSViewRepresentable {
  /// A hash route, `#/shell` or `#/settings?section=general`.
  let route: String
  /// Fixed for the view's lifetime: the coordinator captures it once, so a
  /// window that needs another host makes another view (`.id(...)`).
  let host: any BridgeHost

  func makeCoordinator() -> Coordinator {
    Coordinator(host: host, policy: Self.policy)
  }

  func makeNSView(context: Context) -> WKWebView {
    let coordinator = context.coordinator
    let configuration = WKWebViewConfiguration()
    configuration.websiteDataStore = .nonPersistent()
    configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
    configuration.setURLSchemeHandler(
      coordinator.schemeHandler, forURLScheme: AppSchemeHandler.scheme)
    configuration.userContentController.addScriptMessageHandler(
      coordinator.bridge, contentWorld: .page, name: WebBridge.messageHandlerName)

    let webView: WKWebView = CanvasWebView(frame: .zero, configuration: configuration)
    webView.navigationDelegate = coordinator
    webView.allowsBackForwardNavigationGestures = false
    webView.allowsMagnification = false
    webView.underPageBackgroundColor = .clear
    // Key-value coded: WebKit has no public switch for the view's own
    // background on macOS, and this is the established way to a web view that
    // never flashes white before the page paints (plan, Deviations). Guarded so
    // a WebKit that drops the private setter falls back to the window's own
    // background, which `CanvasWebView` paints in the page's colour anyway.
    if webView.responds(to: NSSelectorFromString("_setDrawsBackground:")) {
      webView.setValue(false, forKey: "drawsBackground")
    }
    #if DEBUG
      webView.isInspectable = true
    #endif

    coordinator.bridge.attach(to: webView)
    host.attach(coordinator.bridge)
    UITestDiagnostics.note("web view created for \(route)")
    coordinator.load(route, in: webView)
    return webView
  }

  func updateNSView(_ webView: WKWebView, context: Context) {
    context.coordinator.load(route, in: webView)
  }

  /// Debug builds may point at the dev server; release builds serve the
  /// bundle whatever the environment says.
  private static var policy: WebNavigationPolicy {
    #if DEBUG
      return WebNavigationPolicy.fromEnvironment(ProcessInfo.processInfo.environment)
    #else
      return .bundled
    #endif
  }

  /// Owns the scheme handler and the bridge for the web view's lifetime and
  /// enforces the navigation policy.
  @MainActor
  final class Coordinator: NSObject, WKNavigationDelegate {
    let schemeHandler: AppSchemeHandler
    let bridge: WebBridge
    let policy: WebNavigationPolicy
    private var loadedRoute: String?

    init(host: any BridgeHost, policy: WebNavigationPolicy) {
      schemeHandler = AppSchemeHandler(site: .bundled())
      bridge = WebBridge(host: host)
      self.policy = policy
    }

    /// Loads the route once; SwiftUI calls `updateNSView` often and a
    /// reload would reset the page's state.
    func load(_ route: String, in webView: WKWebView) {
      guard route != loadedRoute else { return }
      loadedRoute = route
      webView.load(URLRequest(url: policy.startURL(route: route)))
    }

    func webView(
      _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction
    ) async -> WKNavigationActionPolicy {
      guard let url = navigationAction.request.url, policy.allows(url) else { return .cancel }
      return .allow
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
      UITestDiagnostics.note("web view finished \(webView.url?.absoluteString ?? "-")")
    }

    func webView(
      _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
      withError error: any Error
    ) {
      UITestDiagnostics.note("web view failed to load: \(error)")
    }

    func webView(
      _ webView: WKWebView, didFail navigation: WKNavigation!, withError error: any Error
    ) {
      UITestDiagnostics.note("web view failed: \(error)")
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
      UITestDiagnostics.note("web content process terminated")
    }
  }
}

/// The page's canvas colour on the host side: `--background` in
/// `apps/macos/web/src/theme.css`, light on `:root` and dark on `.dark`.
/// `ThemeTokensTests` reads the CSS and fails when the two drift. The
/// native surfaces keep their own `Theme.background` (the mobile ladder);
/// this is the one value the host shares with the pages.
enum WebCanvas {
  static let token = Theme.Token(
    cssName: "web-background", dark: Theme.hex(0x0A0A0A), light: Theme.hex(0xFCFCFC))

  /// Dynamic: resolves per the window's effective appearance when drawn.
  static var color: NSColor { token.nsColor }
}

/// A web view that paints the window behind it in the page's canvas colour
/// when it joins a window and whenever the window's appearance changes
/// (plan Decision 1). The colour is dynamic, so a resize or an appearance
/// switch never shows the system window background under the transparent
/// page.
final class CanvasWebView: WKWebView {
  override func viewDidMoveToWindow() {
    super.viewDidMoveToWindow()
    paintWindow()
  }

  override func viewDidChangeEffectiveAppearance() {
    super.viewDidChangeEffectiveAppearance()
    paintWindow()
  }

  private func paintWindow() {
    window?.backgroundColor = WebCanvas.color
  }
}
