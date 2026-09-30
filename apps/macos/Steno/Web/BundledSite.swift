import Foundation

/// The bundled web UI as `AppSchemeHandler` serves it: a request path under
/// the `Web/` resource folder (copied there by `scripts/build-web.sh`)
/// becomes a file URL, a content type and the response headers. Pure, so the
/// traversal and fallback rules test hostlessly; the WebKit plumbing lives in
/// the handler.
///
/// Plan: `.plans/2026-09-29-macos-webview-ui.md`, Decision 3.
struct BundledSite: Equatable, Sendable {
  struct Resource: Equatable, Sendable {
    var fileURL: URL
    var contentType: String
  }

  /// Decision 3's policy. Attached to every response, the 404 included, so
  /// no document from this origin ever runs without it.
  static let contentSecurityPolicy =
    "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; "
    + "img-src 'self' data: blob:; font-src 'self'; connect-src 'none'"

  /// Everything Vite emits for this app. Deliberately closed: a file with
  /// another extension is a 404, not a guess.
  static let contentTypes: [String: String] = [
    "html": "text/html; charset=utf-8",
    "js": "text/javascript; charset=utf-8",
    "css": "text/css; charset=utf-8",
    "json": "application/json; charset=utf-8",
    "svg": "image/svg+xml",
    "png": "image/png",
    "woff2": "font/woff2",
    "woff": "font/woff",
    "ico": "image/x-icon",
    "txt": "text/plain; charset=utf-8",
    "map": "application/json; charset=utf-8",
  ]

  static let indexFile = "index.html"

  /// The `Web/` folder in the app's resources.
  static func bundled(in bundle: Bundle = .main) -> BundledSite {
    let resources = bundle.resourceURL ?? bundle.bundleURL
    return BundledSite(root: resources.appendingPathComponent("Web", isDirectory: true))
  }

  let root: URL

  init(root: URL) {
    self.root = root.standardizedFileURL
  }

  /// The file for a percent-decoded request path. `/` and any path whose last
  /// component has no extension are the page itself (the router reads the
  /// hash), so a reload at a route never 404s. Anything that could leave the
  /// root (`..`, `.`, hidden files, empty paths after decoding) is refused
  /// before the file system is consulted.
  func resolve(path: String) -> Resource? {
    let components = path.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
    for component in components {
      if component == "." || component == ".." || component.hasPrefix(".")
        || component.contains("\0")
      {
        return nil
      }
    }

    var relative = components
    if Self.fileExtension(of: relative.last) == nil {
      relative = [Self.indexFile]
    }
    guard let ext = Self.fileExtension(of: relative.last),
      let contentType = Self.contentTypes[ext]
    else { return nil }

    var fileURL = root
    for component in relative {
      fileURL.appendPathComponent(component)
    }
    fileURL = fileURL.standardizedFileURL
    guard fileURL.path.hasPrefix(root.path + "/") else { return nil }
    return Resource(fileURL: fileURL, contentType: contentType)
  }

  /// Headers for a served file. `no-store`: the bundle changes with every
  /// build and the data store is non-persistent anyway.
  func headers(for resource: Resource, length: Int) -> [String: String] {
    var headers = Self.baseHeaders
    headers["Content-Type"] = resource.contentType
    headers["Content-Length"] = String(length)
    return headers
  }

  /// Headers for the 404 body, which is plain text.
  static func notFoundHeaders(length: Int) -> [String: String] {
    var headers = baseHeaders
    headers["Content-Type"] = contentTypes["txt"]
    headers["Content-Length"] = String(length)
    return headers
  }

  /// The lowercased extension of a file name, `nil` when it has none.
  private static func fileExtension(of name: String?) -> String? {
    guard let name, let dot = name.lastIndex(of: "."), dot != name.startIndex,
      name.index(after: dot) != name.endIndex
    else { return nil }
    return name[name.index(after: dot)...].lowercased()
  }

  private static let baseHeaders: [String: String] = [
    "Cache-Control": "no-store",
    "Content-Security-Policy": contentSecurityPolicy,
  ]
}
