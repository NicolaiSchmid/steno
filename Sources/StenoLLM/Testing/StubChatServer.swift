import Foundation
import StenoCore
import Synchronization

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif
#if canImport(Glibc)
  import Glibc
#elseif canImport(Darwin)
  import Darwin
#endif

/// One request the stub server accepted, parsed far enough for assertions.
public struct RecordedRequest: Sendable {
  /// Zero-based arrival order.
  public var index: Int
  public var method: String
  public var path: String
  /// Header names lowercased.
  public var headers: [String: String]
  public var body: Data
  /// The body decoded as a chat completion request, when it is one.
  var chat: ChatCompletionRequest?
  /// The `X-Steno-Purpose` header the client sends with every completion.
  public var purpose: String?
  /// Requests in flight (including this one) when it arrived.
  public var inFlightOnArrival: Int

  public var authorization: String? { headers["authorization"] }
}

/// What the stub server answers one request with.
public struct StubResponse: Sendable {
  public enum Behaviour: Sendable, Equatable {
    /// Write status, headers and body, then close.
    case respond
    /// Keep the connection open without answering until `stop()`; for
    /// timeout and cancellation tests.
    case hang
    /// Close the connection without writing anything; a transport error.
    case drop
  }

  public var status: Int
  public var headers: [String: String]
  public var body: Data
  public var behaviour: Behaviour

  public init(
    status: Int = 200, headers: [String: String] = [:], body: Data = Data(),
    behaviour: Behaviour = .respond
  ) {
    self.status = status
    self.headers = headers
    self.body = body
    self.behaviour = behaviour
  }

  public static func json<T: Encodable>(
    _ value: T, status: Int = 200, headers: [String: String] = [:]
  ) -> StubResponse {
    let body = (try? WireJSON.encode(value)) ?? Data()
    var headers = headers
    headers["Content-Type"] = "application/json"
    return StubResponse(status: status, headers: headers, body: body)
  }

  public static let hang = StubResponse(behaviour: .hang)
  public static let drop = StubResponse(behaviour: .drop)
}

/// A loopback HTTP/1.1 server for tests: ephemeral port on 127.0.0.1,
/// scripted responses consumed in arrival order, a fallback responder for
/// path-dependent answers, every request recorded with the number of
/// requests in flight on arrival, and a latch that holds responses so tests
/// can observe concurrency. POSIX sockets and one thread per connection, so
/// the same code runs on macOS and on the Linux container; no wall-clock
/// waits anywhere a test can see.
public final class StubChatServer: Sendable {
  private struct State {
    var queue: [StubResponse] = []
    var responder: (@Sendable (RecordedRequest) -> StubResponse?)?
    var requests: [RecordedRequest] = []
    var inFlight = 0
    var maxInFlight = 0
    var holding = false
    var stopped = false
    var waiters: [(count: Int, continuation: CheckedContinuation<Void, Never>)] = []
  }

  private let state = Mutex(State())
  private let listenFD: Int32
  public let port: UInt16
  /// `http://127.0.0.1:<port>/v1`.
  public let baseURL: URL
  private let latch = NSCondition()

  public init() throws {
    #if canImport(Glibc)
      let streamType = Int32(SOCK_STREAM.rawValue)
    #else
      let streamType = SOCK_STREAM
    #endif
    let fd = socket(AF_INET, streamType, 0)
    guard fd >= 0 else { throw StubServerError(call: "socket", errno: errno) }
    var reuse: Int32 = 1
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))
    var address = sockaddr_in()
    address.sin_family = sa_family_t(AF_INET)
    address.sin_port = 0
    address.sin_addr = in_addr(s_addr: UInt32(0x7F00_0001).bigEndian)
    let bound = withUnsafePointer(to: &address) { pointer in
      pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockaddrPointer in
        bind(fd, sockaddrPointer, socklen_t(MemoryLayout<sockaddr_in>.size))
      }
    }
    guard bound == 0 else {
      close(fd)
      throw StubServerError(call: "bind", errno: errno)
    }
    guard listen(fd, 16) == 0 else {
      close(fd)
      throw StubServerError(call: "listen", errno: errno)
    }
    var boundAddress = sockaddr_in()
    var length = socklen_t(MemoryLayout<sockaddr_in>.size)
    _ = withUnsafeMutablePointer(to: &boundAddress) { pointer in
      pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockaddrPointer in
        getsockname(fd, sockaddrPointer, &length)
      }
    }
    let port = UInt16(bigEndian: boundAddress.sin_port)
    guard let baseURL = URL(string: "http://127.0.0.1:\(port)/v1") else {
      close(fd)
      throw StubServerError(call: "URL", errno: EINVAL)
    }
    listenFD = fd
    self.port = port
    self.baseURL = baseURL
    let thread = Thread { [self] in self.acceptLoop() }
    thread.name = "StubChatServer.accept"
    thread.start()
  }

  deinit {
    stop()
  }

  // MARK: Scripting

  /// Appends responses; the queue is consumed in arrival order.
  public func enqueue(_ responses: StubResponse...) {
    state.withLock { $0.queue.append(contentsOf: responses) }
  }

  /// Consulted when the queue is empty; nil falls through to a 404.
  public func respond(with responder: @escaping @Sendable (RecordedRequest) -> StubResponse?) {
    state.withLock { $0.responder = responder }
  }

  /// While held, every request is recorded and then parked until
  /// `release()`; in-flight counts stay observable meanwhile.
  public func holdResponses() {
    state.withLock { $0.holding = true }
  }

  public func release() {
    state.withLock { $0.holding = false }
    wakeParked()
  }

  /// Waiters check their predicate and call `wait()` while holding `latch`,
  /// so taking it here before broadcasting means no wakeup is ever lost.
  private func wakeParked() {
    latch.lock()
    latch.broadcast()
    latch.unlock()
  }

  // MARK: Observation

  public var requests: [RecordedRequest] {
    state.withLock { $0.requests }
  }

  public var inFlight: Int {
    state.withLock { $0.inFlight }
  }

  /// The largest number of simultaneously open requests so far.
  public var maxInFlight: Int {
    state.withLock { $0.maxInFlight }
  }

  /// Suspends until at least `count` requests have been recorded. Resumes
  /// immediately when they already have; never polls.
  public func received(atLeast count: Int) async {
    await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
      let satisfied = state.withLock { state -> Bool in
        if state.requests.count >= count || state.stopped { return true }
        state.waiters.append((count, continuation))
        return false
      }
      if satisfied { continuation.resume() }
    }
  }

  /// Closes the listening socket, releases every parked or hanging
  /// connection and wakes every waiter.
  public func stop() {
    let waiters = state.withLock { state -> [CheckedContinuation<Void, Never>] in
      if state.stopped { return [] }
      state.stopped = true
      state.holding = false
      let waiting = state.waiters.map(\.continuation)
      state.waiters.removeAll()
      return waiting
    }
    // The accept thread closes the descriptor once it has seen `stopped`,
    // so a new server can never inherit this number while the old loop
    // still calls `accept` on it.
    shutdown(listenFD, Int32(SHUT_RDWR))
    wakeParked()
    for waiter in waiters { waiter.resume() }
  }

  // MARK: Serving

  private func acceptLoop() {
    defer { close(listenFD) }
    while !state.withLock({ $0.stopped }) {
      var pollFD = pollfd(fd: listenFD, events: Int16(POLLIN), revents: 0)
      let ready = poll(&pollFD, 1, 50)
      guard ready > 0, !state.withLock({ $0.stopped }) else { continue }
      let client = accept(listenFD, nil, nil)
      guard client >= 0 else { continue }
      Self.ignoreSIGPIPE(on: client)
      let thread = Thread { [self] in self.serve(client) }
      thread.name = "StubChatServer.connection"
      thread.start()
    }
  }

  /// A peer that closed early must not kill the test process: Darwin raises
  /// SIGPIPE on `send` unless the socket opts out; Linux takes the flag per
  /// call in `write`.
  private static func ignoreSIGPIPE(on fd: Int32) {
    #if canImport(Darwin)
      var on: Int32 = 1
      setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
    #endif
  }

  private func serve(_ fd: Int32) {
    defer { close(fd) }
    guard let raw = readRequest(fd) else { return }
    let request = record(raw)
    let response = state.withLock { state -> StubResponse in
      if !state.queue.isEmpty { return state.queue.removeFirst() }
      if let responder = state.responder, let scripted = responder(request) { return scripted }
      return .json(
        ChatErrorEnvelope(error: .init(message: "no scripted response for \(request.path)")),
        status: 404)
    }
    latch.lock()
    while state.withLock({ $0.holding && !$0.stopped }) { latch.wait() }
    latch.unlock()
    defer { finish() }
    switch response.behaviour {
    case .drop:
      return
    case .hang:
      latch.lock()
      while !state.withLock({ $0.stopped }) { latch.wait() }
      latch.unlock()
      return
    case .respond:
      write(fd, Self.serialize(response))
    }
  }

  private func record(_ raw: RawRequest) -> RecordedRequest {
    let (request, waiters) = state.withLock {
      state -> (RecordedRequest, [CheckedContinuation<Void, Never>]) in
      state.inFlight += 1
      state.maxInFlight = max(state.maxInFlight, state.inFlight)
      let request = RecordedRequest(
        index: state.requests.count,
        method: raw.method,
        path: raw.path,
        headers: raw.headers,
        body: raw.body,
        chat: try? WireJSON.decode(ChatCompletionRequest.self, from: raw.body),
        purpose: raw.headers["x-steno-purpose"],
        inFlightOnArrival: state.inFlight)
      state.requests.append(request)
      let due = state.waiters.filter { $0.count <= state.requests.count }
      state.waiters.removeAll { $0.count <= state.requests.count }
      return (request, due.map(\.continuation))
    }
    for waiter in waiters { waiter.resume() }
    return request
  }

  private func finish() {
    state.withLock { $0.inFlight -= 1 }
  }

  private struct RawRequest {
    var method: String
    var path: String
    var headers: [String: String]
    var body: Data
  }

  private func readRequest(_ fd: Int32) -> RawRequest? {
    var buffer = Data()
    var chunk = [UInt8](repeating: 0, count: 16_384)
    let terminator = Data("\r\n\r\n".utf8)
    var headerEnd: Range<Data.Index>?
    while headerEnd == nil {
      let count = recv(fd, &chunk, chunk.count, 0)
      guard count > 0 else { return nil }
      buffer.append(contentsOf: chunk[0..<Int(count)])
      headerEnd = buffer.range(of: terminator)
      if buffer.count > 1_048_576 { return nil }
    }
    guard let headerEnd else { return nil }
    let headerText = String(
      decoding: buffer[buffer.startIndex..<headerEnd.lowerBound], as: UTF8.self)
    var lines = headerText.components(separatedBy: "\r\n")
    guard !lines.isEmpty else { return nil }
    let requestLine = lines.removeFirst().split(separator: " ", omittingEmptySubsequences: true)
    guard requestLine.count >= 2 else { return nil }
    var headers: [String: String] = [:]
    for line in lines {
      guard let colon = line.firstIndex(of: ":") else { continue }
      let name = line[..<colon].trimmingCharacters(in: .whitespaces).lowercased()
      let value = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
      headers[name] = value
    }
    let contentLength = headers["content-length"].flatMap { Int($0) } ?? 0
    var body = Data(buffer[headerEnd.upperBound...])
    while body.count < contentLength {
      let count = recv(fd, &chunk, chunk.count, 0)
      guard count > 0 else { return nil }
      body.append(contentsOf: chunk[0..<Int(count)])
    }
    if body.count > contentLength { body = body.prefix(contentLength) }
    return RawRequest(
      method: String(requestLine[0]), path: String(requestLine[1]), headers: headers, body: body)
  }

  private static func serialize(_ response: StubResponse) -> Data {
    let reason = HTTPURLResponse.localizedString(forStatusCode: response.status)
    var head = "HTTP/1.1 \(response.status) \(reason)\r\n"
    var headers = response.headers
    headers["Content-Length"] = "\(response.body.count)"
    headers["Connection"] = "close"
    for (name, value) in headers.sorted(by: { $0.key < $1.key }) {
      head += "\(name): \(value)\r\n"
    }
    head += "\r\n"
    return Data(head.utf8) + response.body
  }

  private func write(_ fd: Int32, _ data: Data) {
    data.withUnsafeBytes { (bytes: UnsafeRawBufferPointer) in
      guard let base = bytes.baseAddress else { return }
      var offset = 0
      while offset < bytes.count {
        #if canImport(Glibc)
          let flags = Int32(MSG_NOSIGNAL)
        #else
          let flags: Int32 = 0
        #endif
        let written = send(fd, base + offset, bytes.count - offset, flags)
        guard written > 0 else { return }
        offset += Int(written)
      }
    }
  }
}

/// The named system call failed with `errno` while the server started.
public struct StubServerError: Error, Sendable, Equatable {
  public var call: String
  public var errno: Int32
}
