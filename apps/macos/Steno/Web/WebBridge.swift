import Foundation
import StenoBridge
import WebKit

/// The host side of the JSON bridge (plan Decision 5). Installed as the
/// `steno` script message handler with a reply, so the page's
/// `webkit.messageHandlers.steno.postMessage({id, method, params})` returns a
/// promise that resolves with the `BridgeReply` envelope. Events go the other
/// way through `window.steno.emit(topic, payload)`, one `evaluateJavaScript`
/// per coalesced change. The decoding, routing and escaping are
/// `BridgeDispatcher`, which tests hostlessly; this class is the WebKit glue.
@MainActor
final class WebBridge: NSObject, WKScriptMessageHandlerWithReply, BridgeEventSink {
  static let messageHandlerName = BridgeDispatcher.messageHandlerName

  private let host: any BridgeHost
  private weak var webView: WKWebView?

  init(host: any BridgeHost) {
    self.host = host
  }

  /// The web view events are evaluated in. Weak: the view owns the
  /// configuration that owns this handler.
  func attach(to webView: WKWebView) {
    self.webView = webView
  }

  /// The promise value is the envelope as a JSON object; the second element
  /// would reject the promise and is never used for contract errors, so the
  /// page always sees a typed `error`.
  func userContentController(
    _ userContentController: WKUserContentController, didReceive message: WKScriptMessage
  ) async -> (Any?, String?) {
    let reply = await BridgeDispatcher.dispatch(message.body, host: host)
    return (BridgeDispatcher.replyObject(for: reply), nil)
  }

  /// Delivers a snapshot. Before the page has installed `window.steno` the
  /// statement throws inside the page and the event is lost, which is fine:
  /// hosts publish on `page.ready`, and every snapshot is full state.
  func emit(_ event: BridgeEvent) {
    do {
      run(try BridgeDispatcher.emitStatement(for: event), topic: event.topic)
    } catch {
      NSLog("WebBridge: \(event.topic.rawValue) could not be encoded: \(error)")
      assertionFailure("unencodable event payload: \(error)")
    }
  }

  func emit(_ topic: BridgeTopic, snapshot: some Encodable) {
    do {
      let payload = try BridgeDispatcher.encoder().encode(snapshot)
      run(BridgeDispatcher.emitStatement(topic: topic, payloadJSON: payload), topic: topic)
    } catch {
      NSLog("WebBridge: \(topic.rawValue) snapshot could not be encoded: \(error)")
      assertionFailure("unencodable snapshot: \(error)")
    }
  }

  private func run(_ statement: String, topic: BridgeTopic) {
    guard let webView else { return }
    webView.evaluateJavaScript(statement) { _, error in
      if let error {
        NSLog("WebBridge: emit \(topic.rawValue) failed: \(error)")
      }
    }
  }
}
