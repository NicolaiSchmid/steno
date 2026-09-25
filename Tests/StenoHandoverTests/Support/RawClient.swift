import Foundation
import NIOCore

@testable import StenoHandover

#if canImport(Network)
  import Network
  import NIOTransportServices
  import Security
#else
  import NIOPosix
#endif

/// A byte-level client for the limit tests, where the assertion is about
/// the connection itself: it writes raw HTTP, parses the one response and
/// reports whether the server closed. Over TLS it pins with the same
/// `PinnedTrustEvaluator` as `LoopbackClient`.
struct RawClient {
  struct Exchange: Sendable {
    var status: Int?
    var headers: [String: String]
    var body: Data
    var closedByServer: Bool
    var raw: Data
  }

  let port: UInt16
  let fingerprint: Data

  /// Sends `request`, returns once a full response has been read (status
  /// line, headers, `Content-Length` body) and the server either closed or
  /// `closeGrace` passed. A response that never arrives within `timeout`
  /// returns whatever was read.
  func exchange(
    _ request: Data, closeGrace: Duration = .milliseconds(500), timeout: Duration = .seconds(10)
  ) async throws -> Exchange {
    let collector = Collector()
    #if canImport(Network)
      let group = NIOTSEventLoopGroup(loopCount: 1)
      let tls = NWProtocolTLS.Options()
      let pinned = fingerprint
      sec_protocol_options_set_verify_block(
        tls.securityProtocolOptions,
        { _, secTrust, complete in
          let trust = sec_trust_copy_ref(secTrust).takeRetainedValue()
          complete(PinnedTrustEvaluator.evaluate(trust, pinnedFingerprint: pinned))
        }, DispatchQueue(label: "steno.rawclient.verify"))
      let bootstrap = NIOTSConnectionBootstrap(group: group).tlsOptions(tls)
    #else
      let group = MultiThreadedEventLoopGroup(numberOfThreads: 1)
      let bootstrap = ClientBootstrap(group: group)
    #endif
    defer { Task { try? await group.shutdownGracefully() } }
    let channel = try await bootstrap.channelInitializer { channel in
      channel.eventLoop.makeCompletedFuture {
        try channel.pipeline.syncOperations.addHandler(CollectingHandler(collector: collector))
      }
    }.connectTimeout(.seconds(30)).connect(host: "127.0.0.1", port: Int(port)).get()

    var buffer = channel.allocator.buffer(capacity: request.count)
    buffer.writeBytes(request)
    try await channel.writeAndFlush(buffer)

    let clock = ContinuousClock()
    let deadline = clock.now + timeout
    while clock.now < deadline, !collector.closed, collector.parse() == nil {
      try await Task.sleep(for: .milliseconds(10))
    }
    let graceDeadline = clock.now + closeGrace
    while clock.now < graceDeadline, !collector.closed {
      try await Task.sleep(for: .milliseconds(10))
    }
    let closed = collector.closed
    try? await channel.close()
    let raw = collector.bytes
    let parsed = collector.parse()
    return Exchange(
      status: parsed?.status, headers: parsed?.headers ?? [:], body: parsed?.body ?? Data(),
      closedByServer: closed, raw: raw)
  }

  /// `PUT /path HTTP/1.1` with `Host`, the given headers and `body`.
  static func request(
    _ method: String, _ path: String, headers: [(String, String)] = [], body: Data = Data()
  ) -> Data {
    var text = "\(method) \(path) HTTP/1.1\r\nHost: 127.0.0.1\r\n"
    for (name, value) in headers {
      text += "\(name): \(value)\r\n"
    }
    text += "\r\n"
    return Data(text.utf8) + body
  }
}

/// What the connection has delivered so far, shared between the NIO handler
/// (event loop) and the test (cooperative pool).
final class Collector: @unchecked Sendable {
  private let lock = NSLock()
  private var buffer = Data()
  private var isClosed = false

  var bytes: Data { lock.withLock { buffer } }
  var closed: Bool { lock.withLock { isClosed } }

  func append(_ data: Data) {
    lock.withLock { buffer.append(data) }
  }

  func markClosed() {
    lock.withLock { isClosed = true }
  }

  struct Parsed {
    var status: Int
    var headers: [String: String]
    var body: Data
  }

  /// One HTTP/1.1 response when it is complete, else nil.
  func parse() -> Parsed? {
    let data = bytes
    guard let separator = data.range(of: Data("\r\n\r\n".utf8)) else { return nil }
    let head = String(decoding: data[data.startIndex..<separator.lowerBound], as: UTF8.self)
    let lines = head.components(separatedBy: "\r\n")
    guard let statusLine = lines.first else { return nil }
    let statusParts = statusLine.split(separator: " ", maxSplits: 2)
    guard statusParts.count >= 2, let status = Int(statusParts[1]) else { return nil }
    var headers: [String: String] = [:]
    for line in lines.dropFirst() {
      guard let colon = line.firstIndex(of: ":") else { continue }
      headers[line[..<colon].lowercased()] = line[line.index(after: colon)...].trimmingCharacters(
        in: .whitespaces)
    }
    let length = headers["content-length"].flatMap(Int.init) ?? 0
    let body = data[separator.upperBound...]
    guard body.count >= length else { return nil }
    return Parsed(status: status, headers: headers, body: Data(body.prefix(length)))
  }
}

final class CollectingHandler: ChannelInboundHandler {
  typealias InboundIn = ByteBuffer

  private let collector: Collector

  init(collector: Collector) {
    self.collector = collector
  }

  func channelRead(context: ChannelHandlerContext, data: NIOAny) {
    let buffer = unwrapInboundIn(data)
    collector.append(Data(buffer.readableBytesView))
  }

  func channelInactive(context: ChannelHandlerContext) {
    collector.markClosed()
    context.fireChannelInactive()
  }

  func errorCaught(context: ChannelHandlerContext, error: any Error) {
    collector.markClosed()
    context.close(promise: nil)
  }
}
