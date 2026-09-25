import Foundation
import Network
import dnssd

/// `NWBrowser` over `_steno._tcp` with TXT records, plus one-shot resolution
/// of a service name to a host and port through a throwaway `NWConnection`.
///
/// Browsing and resolving both need the Local Network privilege (TN3179). A
/// denial surfaces as `.waiting(.dns(kDNSServiceErr_PolicyDenied))` and is
/// reported to JS as `browserState.policyDenied`, so the UI can point at
/// Settings instead of spinning forever.
final class Browser {
  typealias EventSink = (_ name: String, _ body: [String: Any]) -> Void

  static let serviceType = "_steno._tcp"

  private let queue = DispatchQueue(label: "uno.schmid.steno.link.browser")
  private let emit: EventSink
  private var browser: NWBrowser?
  /// Latest result per service instance name; `resolve` looks endpoints up here.
  private var resultsByName: [String: NWBrowser.Result] = [:]
  private var pendingResolutions: [UUID: NWConnection] = [:]

  init(emit: @escaping EventSink) {
    self.emit = emit
  }

  func start() {
    queue.async {
      guard self.browser == nil else { return }
      let parameters = NWParameters.tcp
      parameters.includePeerToPeer = false
      let descriptor = NWBrowser.Descriptor.bonjourWithTXTRecord(type: Browser.serviceType, domain: nil)
      let browser = NWBrowser(for: descriptor, using: parameters)
      browser.stateUpdateHandler = { [weak self] state in
        self?.handle(state: state)
      }
      browser.browseResultsChangedHandler = { [weak self] _, changes in
        self?.handle(changes: changes)
      }
      self.browser = browser
      browser.start(queue: self.queue)
    }
  }

  func stop() {
    queue.async {
      self.browser?.cancel()
      self.browser = nil
      self.resultsByName.removeAll()
      for connection in self.pendingResolutions.values {
        connection.cancel()
      }
      self.pendingResolutions.removeAll()
    }
  }

  /// Resolves a browsed service to `host` and `port`. Times out after
  /// `timeout` seconds; the connection is cancelled as soon as the path is known.
  func resolve(
    serviceName: String,
    timeout: TimeInterval = 10,
    completion: @escaping (Result<(host: String, port: Int), Error>) -> Void
  ) {
    queue.async {
      guard let result = self.resultsByName[serviceName] else {
        completion(.failure(BrowserError.unknownService(serviceName)))
        return
      }
      let token = UUID()
      // IPv4 only: a link-local IPv6 address needs its `%zone` to route, and
      // URLs cannot carry one, so an IPv6 result would leave every later
      // request with no route to host on a LAN without an IPv6 router.
      let parameters = NWParameters.tcp
      if let ip = parameters.defaultProtocolStack.internetProtocol as? NWProtocolIP.Options {
        ip.version = .v4
      }
      let connection = NWConnection(to: result.endpoint, using: parameters)
      var finished = false
      let finish: (Result<(host: String, port: Int), Error>) -> Void = { [weak self] outcome in
        guard !finished else { return }
        finished = true
        connection.cancel()
        self?.pendingResolutions.removeValue(forKey: token)
        completion(outcome)
      }
      connection.stateUpdateHandler = { state in
        switch state {
        case .ready:
          if let endpoint = connection.currentPath?.remoteEndpoint,
            let resolved = Browser.hostPort(from: endpoint)
          {
            finish(.success(resolved))
          } else {
            finish(.failure(BrowserError.unresolvable(serviceName)))
          }
        case .failed(let error):
          finish(.failure(error))
        case .cancelled:
          finish(.failure(BrowserError.cancelled))
        default:
          break
        }
      }
      self.pendingResolutions[token] = connection
      connection.start(queue: self.queue)
      self.queue.asyncAfter(deadline: .now() + timeout) {
        finish(.failure(BrowserError.timeout(serviceName)))
      }
    }
  }

  // MARK: - Browser callbacks

  private func handle(state: NWBrowser.State) {
    switch state {
    case .ready:
      emit("browserState", ["state": "ready", "policyDenied": false])
    case .waiting(let error):
      emit("browserState", ["state": "waiting", "policyDenied": Browser.isPolicyDenied(error)])
    case .failed(let error):
      emit("browserState", ["state": "failed", "policyDenied": Browser.isPolicyDenied(error)])
    case .cancelled:
      emit("browserState", ["state": "cancelled", "policyDenied": false])
    case .setup:
      break
    @unknown default:
      break
    }
  }

  private func handle(changes: Set<NWBrowser.Result.Change>) {
    for change in changes {
      switch change {
      case .added(let result):
        remember(result)
        emit("serviceFound", Browser.serviceBody(result))
      case .removed(let result):
        if let name = Browser.serviceName(of: result) {
          resultsByName.removeValue(forKey: name)
        }
        emit("serviceLost", Browser.serviceBody(result))
      case .changed(_, let new, _):
        // TXT or interface changed; the identity in TXT may have moved.
        remember(new)
        emit("serviceFound", Browser.serviceBody(new))
      case .identical:
        break
      @unknown default:
        break
      }
    }
  }

  private func remember(_ result: NWBrowser.Result) {
    if let name = Browser.serviceName(of: result) {
      resultsByName[name] = result
    }
  }

  // MARK: - Helpers

  /// `kDNSServiceErr_PolicyDenied` (-65570): the Local Network privilege was denied.
  static func isPolicyDenied(_ error: NWError) -> Bool {
    if case .dns(let code) = error {
      return code == DNSServiceErrorType(kDNSServiceErr_PolicyDenied)
    }
    return false
  }

  static func serviceName(of result: NWBrowser.Result) -> String? {
    if case .service(let name, _, _, _) = result.endpoint {
      return name
    }
    return nil
  }

  static func macID(of result: NWBrowser.Result) -> String? {
    if case .bonjour(let txt) = result.metadata {
      return txt.dictionary["id"]
    }
    return nil
  }

  static func serviceBody(_ result: NWBrowser.Result) -> [String: Any] {
    return [
      "name": serviceName(of: result) ?? "",
      "macID": macID(of: result) as Any,
    ]
  }

  /// Turns a resolved remote endpoint into a URL-safe host literal and a port.
  static func hostPort(from endpoint: NWEndpoint) -> (host: String, port: Int)? {
    guard case .hostPort(let host, let port) = endpoint else { return nil }
    switch host {
    case .ipv4(let address):
      return ("\(address)", Int(port.rawValue))
    case .ipv6(let address):
      // Not expected with the IPv4-only parameters above; kept for a
      // routable (global) IPv6 result. `IPv6Address.description` appends
      // `%interface` for link-local addresses, which URLs cannot carry.
      let literal = "\(address)".split(separator: "%").first.map(String.init) ?? "\(address)"
      return ("[\(literal)]", Int(port.rawValue))
    case .name(let name, _):
      return (name, Int(port.rawValue))
    @unknown default:
      return nil
    }
  }
}

enum BrowserError: LocalizedError {
  case unknownService(String)
  case unresolvable(String)
  case timeout(String)
  case cancelled

  var errorDescription: String? {
    switch self {
    case .unknownService(let name): return "Service \(name) is not being browsed"
    case .unresolvable(let name): return "Service \(name) resolved to no host"
    case .timeout(let name): return "Resolving \(name) timed out"
    case .cancelled: return "Resolution cancelled"
    }
  }
}
