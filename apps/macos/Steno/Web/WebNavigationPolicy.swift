import Foundation

/// An origin as the navigation policy compares it: scheme and host without
/// case, port exactly.
struct WebOrigin: Equatable, Sendable {
  var scheme: String
  var host: String
  var port: Int?

  init(scheme: String, host: String, port: Int? = nil) {
    self.scheme = scheme.lowercased()
    self.host = host.lowercased()
    self.port = port
  }

  init?(url: URL) {
    guard let scheme = url.scheme, let host = url.host else { return nil }
    self.init(scheme: scheme, host: host, port: url.port)
  }
}

/// Where the web view may navigate (plan Decision 3): the app's own origin,
/// served by `AppSchemeHandler`, and in Debug the Vite dev server when
/// `STENO_WEB_DEV_URL` is set. Every other navigation is cancelled by
/// `WebWindowView`'s delegate; external links go through `system.openURL`.
/// Pure, so the rules test hostlessly.
struct WebNavigationPolicy: Equatable, Sendable {
  static let appScheme = "steno-app"
  static let appHost = "app"
  static let appOrigin = WebOrigin(scheme: appScheme, host: appHost)
  /// The environment variable that switches a Debug build to the dev server.
  static let devServerVariable = "STENO_WEB_DEV_URL"

  /// Production: the bundle only.
  static let bundled = WebNavigationPolicy(devServer: nil)

  /// The dev server's URL when `STENO_WEB_DEV_URL` names one with a scheme
  /// and host; the bundled policy otherwise. Callers gate this on `DEBUG`:
  /// a release build never reads the variable.
  static func fromEnvironment(_ environment: [String: String]) -> WebNavigationPolicy {
    guard let raw = environment[devServerVariable]?.trimmingCharacters(in: .whitespacesAndNewlines),
      !raw.isEmpty
    else { return bundled }
    guard let url = URL(string: raw), WebOrigin(url: url) != nil else {
      #if DEBUG
        NSLog("\(devServerVariable) is not a URL with a scheme and host: \(raw)")
      #endif
      return bundled
    }
    return WebNavigationPolicy(devServer: url)
  }

  /// The dev server, or `nil` for the bundle.
  var devServer: URL?

  /// The origins a navigation may target. `about:blank` is WebKit's own
  /// empty document and has no origin to compare.
  var origins: [WebOrigin] {
    var origins = [Self.appOrigin]
    if let devServer, let origin = WebOrigin(url: devServer) {
      origins.append(origin)
    }
    return origins
  }

  func allows(_ url: URL) -> Bool {
    if url.scheme?.lowercased() == "about", url.absoluteString.lowercased() == "about:blank" {
      return true
    }
    guard let origin = WebOrigin(url: url) else { return false }
    return origins.contains(origin)
  }

  /// The document the window loads for a hash route (`#/shell`,
  /// `#/shell?tab=transcript`): the dev server's root when configured, the
  /// bundled `index.html` otherwise, with the route as the fragment.
  func startURL(route: String) -> URL {
    var components: URLComponents
    if let devServer, let dev = URLComponents(url: devServer, resolvingAgainstBaseURL: false) {
      components = dev
      if components.path.isEmpty { components.path = "/" }
    } else {
      components = URLComponents()
      components.scheme = Self.appScheme
      components.host = Self.appHost
      components.path = "/index.html"
    }
    let fragment = route.hasPrefix("#") ? String(route.dropFirst()) : route
    components.fragment = fragment.isEmpty ? nil : fragment
    // Both branches produce a well-formed URL; the fallback only exists so
    // the call site does not have to unwrap.
    return components.url ?? URL(string: "\(Self.appScheme)://\(Self.appHost)/index.html")!
  }
}
