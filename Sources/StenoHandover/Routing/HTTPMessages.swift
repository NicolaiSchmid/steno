import Foundation
import NIOHTTP1
import StenoCore

/// Who a request comes from, decided at the request head before the body.
enum Principal: Sendable, Equatable {
  case anonymous
  case pairing
  case device(PairedDevice)
}

/// The outcome of the auth gate. A rejection carries the answer the handler
/// writes before it reads the body, so every status decision of the wire is
/// the engine's (`HandoverEngine.unauthorized`, `.pairingRejected`).
enum AuthOutcome: Sendable, Equatable {
  case allowed(Principal)
  case rejected(HandoverResponse)
}

/// One complete, authenticated request handed from the NIO handler to the
/// engine.
struct HandoverRequest: Sendable {
  var route: Route
  var principal: Principal
  var headers: HTTPHeaders
  var body: Data

  /// The device behind a bearer route; routes with other auth never ask.
  var device: PairedDevice? {
    if case .device(let device) = principal { return device }
    return nil
  }
}

struct HandoverResponse: Sendable, Equatable {
  var status: HTTPResponseStatus
  var headers = HTTPHeaders()
  var body = Data()

  static func json<T: Encodable>(_ status: HTTPResponseStatus, _ value: T) -> HandoverResponse {
    do {
      return HandoverResponse(
        status: status, headers: ["Content-Type": "application/json"],
        body: try StenoJSON.encode(value))
    } catch {
      return internalError("encoding the response", error)
    }
  }

  static func empty(_ status: HTTPResponseStatus) -> HandoverResponse {
    HandoverResponse(status: status)
  }

  static func problem(_ status: HTTPResponseStatus, _ message: String) -> HandoverResponse {
    json(status, Wire.Problem(message))
  }

  /// A 500 whose body names the step and nothing else; the error itself,
  /// which may carry a path under the user's home, goes to the local log.
  static func internalError(_ what: String, _ error: any Error) -> HandoverResponse {
    HandoverLog.failure(what, error)
    return problem(.internalServerError, "\(what) failed on the Mac")
  }
}

/// The engine as the NIO handler sees it: an auth gate that runs before the
/// body, and a request handler that runs after it.
protocol RequestHandling: Sendable {
  func authenticate(_ route: Route, authorization: String?) async -> AuthOutcome
  func handle(_ request: HandoverRequest) async -> HandoverResponse
}
