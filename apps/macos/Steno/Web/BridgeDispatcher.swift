import Foundation
import StenoBridge
import StenoCore

/// Answers the page's commands for one window. WP2 onwards gives each window
/// a host that maps methods onto its view models; `PreviewBridgeHost` is the
/// Debug stand-in. Throw a `BridgeError` for a contract problem (unknown
/// method, bad params, missing meeting); any other error reaches the page as
/// `failed`.
@MainActor
protocol BridgeHost: AnyObject {
  /// The reply value, or `nil` for methods without one.
  func handle(_ request: BridgeRequest) async throws -> JSONValue?
  /// Called once the web view exists. Hosts keep the sink to publish
  /// snapshots; the default keeps nothing.
  func attach(_ events: any BridgeEventSink)
}

extension BridgeHost {
  func attach(_ events: any BridgeEventSink) {}
}

/// Where a host publishes snapshots. `WebBridge` is the live one; tests
/// record.
@MainActor
protocol BridgeEventSink: AnyObject {
  func emit(_ event: BridgeEvent)
}

/// The pure half of `WebBridge`: the message body from
/// `webkit.messageHandlers.steno.postMessage` in, a `BridgeReply` out, and
/// the JavaScript statement that delivers an event. The wire shapes are the
/// contract's `BridgeRequest` and `BridgeReply` envelopes exactly as
/// `apps/macos/web/src/bridge/webkit-transport.ts` posts and reads them; a
/// body the host cannot read still gets a reply, so the page's promise always
/// settles with a typed error instead of rejecting.
///
/// Plan: `.plans/2026-09-29-macos-webview-ui.md`, Decisions 5 and 6.
enum BridgeDispatcher {
  /// The `webkit.messageHandlers.<name>` the page posts to.
  static let messageHandlerName = "steno"

  enum Decoding: Equatable {
    case request(BridgeRequest)
    /// The reply to send instead; `id` is the body's when it was readable.
    case rejected(BridgeReply)
  }

  /// `BridgeJSON`'s convention on one line: emit statements and replies are
  /// never read by a person.
  static func encoder() -> JSONEncoder {
    let encoder = BridgeJSON.encoder()
    encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
    return encoder
  }

  /// Reads the envelope from a script message body (a JavaScript object,
  /// bridged as a dictionary). A method name outside the contract is
  /// `unknownMethod`; anything else that fails to decode is `invalidParams`.
  static func decode(_ body: Any?) -> Decoding {
    guard let object = body as? [String: Any] else {
      return .rejected(
        BridgeReply(
          id: "",
          error: BridgeError(code: .invalidParams, message: "The message is not an object.")))
    }
    let id = object["id"] as? String ?? ""
    guard JSONSerialization.isValidJSONObject(object),
      let data = try? JSONSerialization.data(withJSONObject: object)
    else {
      return .rejected(
        BridgeReply(
          id: id,
          error: BridgeError(code: .invalidParams, message: "The message is not JSON.")))
    }
    do {
      return .request(try BridgeJSON.decode(BridgeRequest.self, from: data))
    } catch {
      if let method = object["method"] as? String, BridgeMethod(rawValue: method) == nil {
        return .rejected(
          BridgeReply(
            id: id,
            error: BridgeError(code: .unknownMethod, message: "Unknown method '\(method)'.")))
      }
      return .rejected(
        BridgeReply(
          id: id,
          error: BridgeError(
            code: .invalidParams, message: "The message is not a bridge request: \(error)")))
    }
  }

  /// One call end to end: decode, run on the host, wrap the outcome. Never
  /// throws, so the page's promise resolves with an envelope in every case.
  /// On the main actor with the host, so the body (a WebKit object) and the
  /// host never cross an isolation boundary.
  @MainActor
  static func dispatch(_ body: Any?, host: any BridgeHost) async -> BridgeReply {
    let request: BridgeRequest
    switch decode(body) {
    case .request(let decoded): request = decoded
    case .rejected(let reply): return reply
    }
    do {
      return BridgeReply(id: request.id, result: try await host.handle(request))
    } catch let error as BridgeError {
      return BridgeReply(id: request.id, error: error)
    } catch {
      return BridgeReply(
        id: request.id, error: BridgeError(code: .failed, message: "\(error)"))
    }
  }

  /// The reply as WebKit wants it for the promise: a dictionary of JSON
  /// values, never a JSON string. Encoding a `Codable` envelope cannot fail
  /// in practice; when it does, the page still gets an error envelope.
  static func replyObject(for reply: BridgeReply) -> Any {
    do {
      return try JSONSerialization.jsonObject(with: encoder().encode(reply))
    } catch {
      return [
        "id": reply.id,
        "error": [
          "code": BridgeError.Code.failed.rawValue, "message": "The reply could not be encoded.",
        ],
      ] as [String: Any]
    }
  }

  /// `window.steno.emit("topic", JSON.parse("..."))`. The payload travels as
  /// a JSON string literal rather than inline object syntax: one escaping
  /// rule (JavaScript string) instead of trusting JSON to be a JavaScript
  /// subset, which U+2028 and U+2029 break.
  static func emitStatement(for event: BridgeEvent) throws -> String {
    let payload = String(decoding: try encoder().encode(event.payload), as: UTF8.self)
    return
      "window.steno.emit(\(javaScriptStringLiteral(event.topic.rawValue)), "
      + "JSON.parse(\(javaScriptStringLiteral(payload))))"
  }

  /// A double-quoted JavaScript string literal. Escapes the quote, the
  /// backslash, every C0 control character and the two Unicode line
  /// terminators; everything else is passed through as UTF-8.
  static func javaScriptStringLiteral(_ string: String) -> String {
    var literal = "\""
    for scalar in string.unicodeScalars {
      switch scalar {
      case "\"": literal += "\\\""
      case "\\": literal += "\\\\"
      case "\n": literal += "\\n"
      case "\r": literal += "\\r"
      case "\t": literal += "\\t"
      case "\u{2028}": literal += "\\u2028"
      case "\u{2029}": literal += "\\u2029"
      case "\u{0}"..."\u{1F}":
        let hex = String(scalar.value, radix: 16, uppercase: true)
        literal += "\\u" + String(repeating: "0", count: 4 - hex.count) + hex
      default: literal.unicodeScalars.append(scalar)
      }
    }
    return literal + "\""
  }
}

extension BridgeEvent {
  /// A snapshot as the host publishes it: encoded with the contract's
  /// convention and carried as the event's `JSONValue` payload.
  init(topic: BridgeTopic, snapshot: some Encodable) throws {
    let data = try BridgeDispatcher.encoder().encode(snapshot)
    self.init(topic: topic, payload: try BridgeJSON.decode(JSONValue.self, from: data))
  }
}
