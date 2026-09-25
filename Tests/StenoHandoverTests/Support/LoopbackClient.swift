import Foundation
import StenoCore

@testable import StenoHandover

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

/// The phone as far as the Mac can tell: a `URLSession` client that pins the
/// leaf fingerprint through the very `PinnedTrustEvaluator.swift` the iOS
/// module ships (symlinked into `Support/`), never follows a redirect and
/// speaks the wire's JSON. On Linux the listener is plaintext and the
/// evaluator does not compile, so the client is a plain session.
struct LoopbackClient: Sendable {
  struct Response: Sendable {
    var status: Int
    var headers: [String: String]
    var body: Data

    func json<T: Decodable>(_ type: T.Type) throws -> T {
      try StenoJSON.decode(type, from: body)
    }
  }

  let baseURL: URL
  let fingerprint: Data
  let timeout: TimeInterval

  init(baseURL: URL, fingerprint: Data, timeout: TimeInterval = 15) {
    self.baseURL = baseURL
    self.fingerprint = fingerprint
    self.timeout = timeout
  }

  /// A client for a running service, pinning its identity unless another
  /// fingerprint is given.
  static func forService(_ service: HandoverService, fingerprint: Data? = nil) async throws
    -> LoopbackClient
  {
    guard let url = await service.loopbackURL else { throw ServerError.notListening }
    return LoopbackClient(baseURL: url, fingerprint: fingerprint ?? service.identity.fingerprint)
  }

  func request(
    _ method: String, _ path: String, headers: [String: String] = [:], body: Data? = nil
  ) async throws -> Response {
    var request = URLRequest(url: baseURL.appendingPathComponent(path))
    request.httpMethod = method
    request.timeoutInterval = timeout
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    for (name, value) in headers {
      request.setValue(value, forHTTPHeaderField: name)
    }
    if let body {
      request.httpBody = body
      if request.value(forHTTPHeaderField: "Content-Type") == nil {
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
      }
    }
    let session = makeSession()
    defer { session.finishTasksAndInvalidate() }
    let (data, response) = try await session.data(for: request)
    guard let http = response as? HTTPURLResponse else {
      throw ClientError.notHTTP
    }
    var responseHeaders: [String: String] = [:]
    for (name, value) in http.allHeaderFields {
      if let name = name as? String, let value = value as? String {
        responseHeaders[name.lowercased()] = value
      }
    }
    return Response(status: http.statusCode, headers: responseHeaders, body: data)
  }

  func json<T: Encodable>(
    _ method: String, _ path: String, headers: [String: String] = [:], body: T
  ) async throws -> Response {
    try await request(method, path, headers: headers, body: try StenoJSON.encode(body))
  }

  static func bearer(_ token: String) -> [String: String] { ["Authorization": "Bearer \(token)"] }
  static func pairing(_ secret: Data) -> [String: String] {
    ["Authorization": "Pairing \(secret.base64EncodedString())"]
  }

  private func makeSession() -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.timeoutIntervalForRequest = timeout
    configuration.timeoutIntervalForResource = timeout
    #if canImport(Security)
      return URLSession(
        configuration: configuration, delegate: PinnedDelegate(fingerprint: fingerprint),
        delegateQueue: nil)
    #else
      return URLSession(configuration: configuration)
    #endif
  }

  enum ClientError: Error {
    case notHTTP
  }
}

#if canImport(Security)
  /// The iOS module's `PinnedSessionDelegate`, reduced to what the tests
  /// need: trust through `PinnedTrustEvaluator`, no redirects.
  final class PinnedDelegate: NSObject, URLSessionTaskDelegate {
    private let fingerprint: Data

    init(fingerprint: Data) {
      self.fingerprint = fingerprint
    }

    func urlSession(
      _ session: URLSession,
      didReceive challenge: URLAuthenticationChallenge,
      completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
      let (disposition, credential) = PinnedTrustEvaluator.respond(
        to: challenge, pinnedFingerprint: fingerprint)
      completionHandler(disposition, credential)
    }

    func urlSession(
      _ session: URLSession,
      task: URLSessionTask,
      willPerformHTTPRedirection response: HTTPURLResponse,
      newRequest request: URLRequest,
      completionHandler: @escaping (URLRequest?) -> Void
    ) {
      completionHandler(nil)
    }
  }
#endif
