import Foundation

/// Session delegate for the foreground client: every server-trust challenge
/// goes through `PinnedTrustEvaluator` with the fingerprint of the paired Mac.
/// A rejected pin cancels the task, which URLSession reports as
/// `NSURLErrorCancelled`; `pinRejected` lets the client name the real cause.
final class PinnedSessionDelegate: NSObject, URLSessionTaskDelegate {
  private let fingerprint: Data
  private(set) var pinRejected = false

  init(fingerprint: Data) {
    self.fingerprint = fingerprint
  }

  func urlSession(
    _ session: URLSession,
    didReceive challenge: URLAuthenticationChallenge,
    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
  ) {
    let (disposition, credential) = PinnedTrustEvaluator.respond(to: challenge, pinnedFingerprint: fingerprint)
    if disposition == .cancelAuthenticationChallenge {
      pinRejected = true
    }
    completionHandler(disposition, credential)
  }

  /// Never follow a redirect: it could carry the bearer to a host that was not pinned.
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

/// One small foreground request (hello, pair, announce, status, complete,
/// unpair) over an ephemeral pinned session. Bodies are UTF-8 JSON;
/// `PinnedRequest.fingerprint` is standard base64 of the 32-byte leaf hash.
enum PinnedClient {
  static func perform(_ request: PinnedRequest, completion: @escaping (Result<PinnedResponse, Error>) -> Void) {
    let timeout: TimeInterval = max(request.timeoutMs, 1) / 1000
    guard let url = URL(string: request.url) else {
      completion(.failure(StenoLinkError.badURL(request.url)))
      return
    }
    guard url.scheme?.lowercased() == "https" else {
      completion(.failure(StenoLinkError.notHTTPS(request.url)))
      return
    }
    guard let fingerprint = Data(base64Encoded: request.fingerprint), fingerprint.count == 32 else {
      completion(.failure(StenoLinkError.badFingerprint))
      return
    }

    var urlRequest = URLRequest(url: url)
    urlRequest.httpMethod = request.method
    urlRequest.timeoutInterval = timeout
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
    configuration.timeoutIntervalForRequest = timeout
    configuration.timeoutIntervalForResource = timeout
    let delegate = PinnedSessionDelegate(fingerprint: fingerprint)
    let session = URLSession(configuration: configuration, delegate: delegate, delegateQueue: nil)
    let task = session.dataTask(with: urlRequest) { data, response, error in
      defer { session.finishTasksAndInvalidate() }
      if let error {
        completion(.failure(delegate.pinRejected ? StenoLinkError.pinMismatch : error))
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
      completion(.success(PinnedResponse(status: http.statusCode, headers: headers, body: body)))
    }
    task.resume()
  }
}
