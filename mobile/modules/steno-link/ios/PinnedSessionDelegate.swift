import Foundation

/// Session delegate for the foreground client: every server-trust challenge
/// goes through `PinnedTrustEvaluator` with the fingerprint of the paired Mac.
final class PinnedSessionDelegate: NSObject, URLSessionDelegate {
  private let fingerprint: Data

  init(fingerprint: Data) {
    self.fingerprint = fingerprint
  }

  func urlSession(
    _ session: URLSession,
    didReceive challenge: URLAuthenticationChallenge,
    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
  ) {
    let (disposition, credential) = PinnedTrustEvaluator.respond(to: challenge, pinnedFingerprint: fingerprint)
    completionHandler(disposition, credential)
  }
}

/// One small foreground request (hello, pair, announce, status, complete,
/// unpair) over an ephemeral pinned session. Bodies are UTF-8 JSON.
enum PinnedClient {
  struct Request {
    var url: String
    var method: String
    var headers: [String: String]
    var body: String?
    /// Standard base64 of the 32-byte leaf fingerprint.
    var fingerprint: String
    var timeout: TimeInterval
  }

  struct Response {
    var status: Int
    var headers: [String: String]
    var body: String
  }

  static func perform(_ request: Request, completion: @escaping (Result<Response, Error>) -> Void) {
    guard let url = URL(string: request.url) else {
      completion(.failure(StenoLinkError.badURL(request.url)))
      return
    }
    guard let fingerprint = Data(base64Encoded: request.fingerprint), fingerprint.count == 32 else {
      completion(.failure(StenoLinkError.badFingerprint))
      return
    }

    var urlRequest = URLRequest(url: url)
    urlRequest.httpMethod = request.method
    urlRequest.timeoutInterval = request.timeout
    for (name, value) in request.headers {
      urlRequest.setValue(value, forHTTPHeaderField: name)
    }
    if let body = request.body {
      urlRequest.httpBody = body.data(using: .utf8)
      if urlRequest.value(forHTTPHeaderField: "Content-Type") == nil {
        urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
      }
    }

    let configuration = URLSessionConfiguration.ephemeral
    configuration.waitsForConnectivity = false
    configuration.timeoutIntervalForRequest = request.timeout
    configuration.timeoutIntervalForResource = request.timeout
    let session = URLSession(
      configuration: configuration,
      delegate: PinnedSessionDelegate(fingerprint: fingerprint),
      delegateQueue: nil
    )
    let task = session.dataTask(with: urlRequest) { data, response, error in
      defer { session.finishTasksAndInvalidate() }
      if let error {
        completion(.failure(error))
        return
      }
      guard let http = response as? HTTPURLResponse else {
        completion(.failure(StenoLinkError.notHTTP))
        return
      }
      var headers: [String: String] = [:]
      for (name, value) in http.allHeaderFields {
        if let name = name as? String, let value = value as? String {
          headers[name.lowercased()] = value
        }
      }
      let body = data.flatMap { String(data: $0, encoding: .utf8) } ?? ""
      completion(.success(Response(status: http.statusCode, headers: headers, body: body)))
    }
    task.resume()
  }
}
