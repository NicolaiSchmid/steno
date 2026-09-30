import AppKit
import StenoBridge
import SwiftUI
import WebKit

/// One window's content: a `WKWebView` at a hash route of the bundled web UI
/// (plan Decision 1), talking to `host` through `WebBridge`. The web view has
/// no persistent storage, cannot open windows, paints no background of its
/// own (the page's canvas token shows through the window), and its delegate
/// cancels every navigation off the app's origin. In Debug,
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

    let webView = WKWebView(frame: .zero, configuration: configuration)
    webView.navigationDelegate = coordinator
    webView.allowsBackForwardNavigationGestures = false
    webView.allowsMagnification = false
    webView.underPageBackgroundColor = .clear
    // Key-value coded: WebKit has no public switch for the view's own
    // background on macOS, and this is the established way to a web view that
    // never flashes white before the page paints (plan, Deviations). Guarded so
    // a WebKit that drops the private setter falls back to the window's own
    // background instead of raising an unknown-key exception.
    if webView.responds(to: NSSelectorFromString("_setDrawsBackground:")) {
      webView.setValue(false, forKey: "drawsBackground")
    }
    #if DEBUG
      webView.isInspectable = true
    #endif

    coordinator.bridge.attach(to: webView)
    host.attach(coordinator.bridge)
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
  }
}
