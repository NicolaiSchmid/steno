import Foundation
import WebKit

/// Serves `steno-app://app/...` from the bundled `Web/` folder through
/// `BundledSite` (plan Decision 3). Custom scheme rather than `file://` so
/// relative URLs and the policy header behave, and rather than a local HTTP
/// server so no port is open to other processes. Every response, the 404
/// included, carries the content security policy.
///
/// WebKit calls both methods on the main thread and raises when a task is
/// answered after `stop`, so tasks are kept by identity and a reply that
/// completes after WebKit lost interest is dropped.
@MainActor
final class AppSchemeHandler: NSObject, WKURLSchemeHandler {
  static let scheme = WebNavigationPolicy.appScheme

  private let site: BundledSite
  private var active: [ObjectIdentifier: any WKURLSchemeTask] = [:]

  init(site: BundledSite) {
    self.site = site
  }

  func webView(_ webView: WKWebView, start urlSchemeTask: any WKURLSchemeTask) {
    let key = ObjectIdentifier(urlSchemeTask)
    active[key] = urlSchemeTask
    guard let url = urlSchemeTask.request.url else {
      respondNotFound(key, url: URL(string: "\(Self.scheme)://\(WebNavigationPolicy.appHost)/")!)
      return
    }
    // One origin only: the navigation policy admits `steno-app://app`, and
    // subresource requests never reach the policy, so the handler must not
    // serve the bundle under any other host.
    guard url.host?.lowercased() == WebNavigationPolicy.appHost,
      let resource = site.resolve(path: url.path)
    else {
      respondNotFound(key, url: url)
      return
    }
    // The read leaves the main thread; only the file URL crosses over. The
    // task is checked by identity when the bytes are back: WebKit may have
    // stopped it and reused the address for a newer task in the meantime.
    let fileURL = resource.fileURL
    Task { [weak self] in
      let data = await Task.detached(priority: .userInitiated) {
        try? Data(contentsOf: fileURL)
      }.value
      guard let self, self.active[key] === urlSchemeTask else { return }
      if let data {
        self.respond(
          key, url: url, status: 200, headers: self.site.headers(for: resource, length: data.count),
          body: data)
      } else {
        self.respondNotFound(key, url: url)
      }
    }
  }

  func webView(_ webView: WKWebView, stop urlSchemeTask: any WKURLSchemeTask) {
    active.removeValue(forKey: ObjectIdentifier(urlSchemeTask))
  }

  private func respondNotFound(_ key: ObjectIdentifier, url: URL) {
    let body = Data("Not found".utf8)
    respond(
      key, url: url, status: 404, headers: BundledSite.notFoundHeaders(length: body.count),
      body: body)
  }

  private func respond(
    _ key: ObjectIdentifier, url: URL, status: Int, headers: [String: String], body: Data
  ) {
    guard let task = active.removeValue(forKey: key) else { return }
    guard
      let response = HTTPURLResponse(
        url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)
    else {
      task.didFailWithError(URLError(.badServerResponse))
      return
    }
    task.didReceive(response)
    task.didReceive(body)
    task.didFinish()
  }
}
